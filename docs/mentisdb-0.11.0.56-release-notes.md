# MentisDB 0.11.0.56 (Valid-version retrieval, public and private chains, background migrations)

**Date:** November 2, 2026

## Security notice — upgrade recommended

0.11.0.56 closes several authorization and integrity holes present in 0.10.10.55 and
earlier. Upgrade if any of the following describes your deployment:

- **The daemon runs on a machine where someone browses the web (the default setup).**
  With bearer-token access off (the default), a malicious web page could use DNS
  rebinding or cross-site requests to drive the local REST and MCP listeners, reading
  and writing memory. Browser requests to REST and MCP must now name an allowed host,
  and the MCP listener on a loopback bind refuses every unknown `Host` (see
  [One host policy](#one-host-policy-for-dashboard-rest-and-mcp)). Programmatic clients
  (`curl`, the CLI, agents) are unaffected.
- **You issue chain-scoped bearer tokens.** A chain-scoped token could reach chains
  outside its scope: through alternative chain fields in a request, a blank entry in a
  federated search, chain-scoped MCP tools that the authorizer did not list (it failed
  open to the default chain), and the ancestor chains of a branch search. Authorization
  now checks exactly the chain the handler will use and fails closed, each ancestor of a
  branch needs the token's scope, and `chain_keys` entries are validated. If you hand
  scoped tokens to agents or users you do not fully trust, review recent activity on
  your chains.
- **You rely on agent keys or thought signatures.** Any client that could call the API
  could register a signing key for any agent (with any bytes as the key), revoke keys,
  disable agents or change an agent's status, and stored thought signatures were never
  verified. Key and agent administration is now operator-only (dashboard PIN or local
  CLI), keys must be valid Ed25519 points, and a thought that carries a signature must
  verify against an active key of its agent or the append is refused. Treat the signed
  status of thoughts written before 0.11 as unverified, and after upgrading review the
  registered keys (dashboard, or `mentisdb agent registry`) and revoke any you do not
  recognize with `mentisdb agent key revoke`.
- **Your dashboard is reachable from other machines.** The PIN could be guessed without
  limit through the `Authorization: Bearer` path (only the login form was rate limited).
  Both now share one per-IP limiter (5 failures per 5 minutes). If your dashboard was
  exposed, change `MENTISDB_DASHBOARD_PIN`. Dashboard responses now also carry a
  Content-Security-Policy, `X-Frame-Options: DENY` and `nosniff`, which closes
  clickjacking of the state-changing UI.
- **Other local users share the host.** The bearer-token registry
  (`<MENTISDB_DIR>/bearer-tokens.json`, which holds token aliases, scopes and hashes)
  was written world-readable. New writes create it `0600`; run
  `chmod 600 <MENTISDB_DIR>/bearer-tokens.json` to fix an existing file now.
- **You restore or import chain files you did not write.** A forged length prefix inside
  a chain record or a persisted HNSW graph made the daemon attempt a huge allocation and
  abort, so one crafted file could take the daemon down. Every decode of on-disk bytes now
  runs under a size budget and fails with an error instead. A `thoughts/traverse` request
  with an enormous `chunk_size` could likewise panic the server; the reservation is now
  capped by the chain length.

The MCP transport now uses `cloudllm_mcp` 0.4.0, which closes the 0.3.6 library holes
itself (unauthenticated event stream, prefix-matched `Origin`, empty bearer tokens, CR/LF
in event fields). MentisDB had already worked around each of them, with regression tests
that now exercise the library.

## Security and reliability fixes from the release review

> **PLACEHOLDER — fill in before tagging.** Fixes from the pre-release review land on
> master after this draft; this section lists each one with its user-visible behaviour
> change once the final list is in.

## Highlights

- **Agents get the current version of a memory.** When a policy or decision is corrected,
  search now returns the live successor instead of the old text, flags memories that a
  newer correction contests, and warns at append time when a correction forgets to say
  what it replaces.
- **Public and private chains.** A chain can be public (unsigned writes, free reads) or
  private (signed writes by allowed keys, readable only with an Ed25519 read proof). Mode
  changes are recorded on the chain, only the operator administers keys, and governed
  chains are verified when they open.
- **Background, recoverable migrations.** Format upgrades no longer block startup: they run
  in the background, journaled and backed up, and each step can be inspected, retried,
  skipped or rolled back with `mentisdb migrate`.
- **Faster, safer storage.** The embedding model no longer runs under the chain's write
  lock (lock hold per append about 0.5 ms, from about 9.5 ms), one daemon owns a data
  directory, and chains written by March and April 2026 builds open again.
- **Compact search results**, contributed by Austin Faulkner-Pearce
  ([#25](https://github.com/CloudLLM-ai/mentisdb/pull/25)): about a quarter smaller
  responses with provenance and version fields kept.
- **Better recall on both benchmarks.** LoCoMo-10P R@10 76.7% (was 73.5%) and
  LongMemEval R@5 69.8% (was 68.0%), full protocol with fresh ingest.
- **Standing context and a complete CLI.** Agents load durable preferences, decisions and
  constraints at startup regardless of age, and every MCP tool, REST route and dashboard
  action has a `mentisdb` command.

## Valid-version retrieval

Measured on a real installation, agents almost never linked a correction to the memory it
corrected: none of 14 `Correction`/`AssumptionInvalidated` thoughts and none of about 90
`Decision` thoughts carried a typed `Supersedes`, `Corrects` or `Invalidates` relation (half
the corrections had no link at all, the rest only `refs`). The invalidated set stayed empty,
so an agent asking about a policy could be handed the old policy, which usually won
lexically. 0.11 reasons about lineage at read time (new module `lineage`, derived when a
chain opens; no migration):

- A superseded thought that would have ranked is **replaced by its live successor**; the hit
  says which thought it replaced (`successor_of`).
- A thought that a newer `Correction`, `AssumptionInvalidated` or `Decision` only references,
  or that anything `Contradicts`, is kept but reported `version_status: "contested"`, and the
  contesting thought is ranked above it. Weak links do not hide memory: a consolidation note
  that references six still-valid memories must not make them vanish.
- Every thought JSON, on every surface (search, get, head, recent, traverse, bundles,
  dashboard), carries `version_status`, `successors` and `current_version` when it is not
  plainly current, including `include_invalidated=true` results.
- The append response (REST and MCP) carries `warnings` naming candidate target ids when a
  correction or replacing decision has no typed invalidating relation, so the agent can
  append the missing `Supersedes`. Library: `MentisDb::append_advice`.
- **Cross-chain and branch lineage.** A `Supersedes`, `Corrects` or `Invalidates` relation
  with a `chain_key` applies wherever one read sees both chains: branch search (branch plus
  ancestors) and federated search (`successor_of_chain`, `current_version.chain_key`).
- Context bundles redirect seeds the same way, standing context swaps a superseded
  constraint or decision for its successor, and the MEMORY.md export drops superseded
  thoughts and marks contested ones `contested by #N`.
- An expired supersession (past its `invalid_at`) no longer hides its target.

`RankedSearchQuery::with_resolve_successors(false)` (or `resolve_successors: false` per
request) restores plain hiding. The skill served to agents now says that a policy change
must carry `Supersedes` with the old id, and to read `version_status` instead of retrying
with `include_invalidated`.

### Ranking changes

- **Relative vector fusion** (new default). The vector boost used to decay with the raw BM25
  score, which grows with query length, so a ten-word paraphrase silenced the vector channel
  exactly when the wording differed. It now decays on a hit's lexical score relative to the
  query's best. `MENTISDB_VECTOR_FUSION=absolute` restores the old blend.
- **Result-time duplicate collapse** (Jaccard 0.9, text queries). One real chain had 301 of
  749 live thoughts in near-duplicate clusters. Survivors list the folded ids in
  `collapsed_duplicates`. `MENTISDB_COLLAPSE_DUPLICATES=off` or `collapse_duplicates: 0`
  disables it.
- **Knobs everywhere.** REST, MCP and the CLI accept `resolve_successors`,
  `collapse_duplicates`, `vector_fusion`, `synonym_weight`, `df_scope` and `df_cutoffs.*`;
  bad values are errors, not silent defaults (map in `docs/CLI_PARITY.md`).
  `MENTISDB_DF_SCOPE=field` is available and stays opt-in.
- **Library and server agree.** `RankedSearchQuery::new()` now applies the same defaults as
  the daemon: relative fusion, duplicate collapse, successor redirection and thesaurus
  expansion at weight 0.3 (the library used to apply none).
- **Deprecated:** `RankedSearchQuery::{llm_rerank, llm_model, llm_rerank_top_k,
  with_llm_reranking}`, `RankedSearchScore::llm_score` (LLM reranking was never implemented;
  `llm_score` is always 0) and `search::ppr` (never called by ranked search). All are no-ops
  marked `#[deprecated]` and will be removed in the next breaking release.
- A description, alias or owner added to an agent after its thoughts were written is now
  searchable without a restart, and automatic dedup can no longer retire a thought the
  author rated more important than the new one.

## Public and private chains

A chain is **public** (unsigned writes, free reads) or **private** (writes must be signed by an
allowed key; what was written while private can be read only by a reader who proves
possession of an allowed key). Every mode change is a signed record on the chain that opens a
*segment*, so each thought belongs to exactly one segment by position and the history of who
could read what is auditable. Existing chains are derived at open time and nothing is
rewritten: chains without a policy are public.

An unauthorized reader sees only public segments on every read surface: ranked, lexical,
vector and graph search, recent and standing context, traversal, get/head/genesis, bundles,
federated and branch search, Markdown export, chain and agent listings and counts. Scores do
not move with private writes (BM25 statistics are computed over the visible set), graph
expansion never passes through a hidden thought, and a public reader is never redirected to
a private successor.

```bash
# operator: dashboard PIN, or --offline on the host that holds the data directory
mentisdb chain access set private --chain vault --pin 123456
mentisdb agent key add builder --key-id builder-1 --public-key-hex <64 hex> --chain vault --pin 123456
mentisdb chain access allow builder builder-1 <64 hex> --chain vault --pin 123456

# an authorized reader signs every request locally; the private key never leaves it
mentisdb search "launch plan" --chain vault --read-key-file builder.key --read-agent builder --read-key-id builder-1
MENTISDB_READ_KEY_FILE=builder.key MENTISDB_READ_AGENT_ID=builder MENTISDB_READ_KEY_ID=builder-1 mentisdb --mode stdio
```

- **Read proofs.** `X-Mentis-Auth` is an Ed25519 signature over the method, path, query,
  body hash, key identity, a timestamp and a single-use nonce (60-second window, bounded
  replay cache that fails closed). The CLI, the stdio MCP proxy and pymentisdb's
  `ReadSigner` sign locally, so a private key never has to enter an LLM context.
- **Operator-only administration.** Adding or removing allowed keys, registering or
  revoking agent keys, disabling agents and switching a chain public or private are done from
  the dashboard (behind its PIN) or the local CLI (`mentisdb chain access set|allow|deny|admin`,
  `agent key add|revoke`, `agent disable|activate`, `--offline` against the data directory).
  The operator signs policy records with `<data dir>/operator/operator.key`, which is always a
  governor, so a chain cannot be locked out of its own administration. The REST routes
  `/v1/agents/keys`, `/v1/agents/keys/revoke`, `/v1/agents/disable` and
  `/v1/access/policy/set` answer `403` with any token, and the matching MCP tools are no longer
  offered.
- **Open-time verification.** Thoughts in a private segment must carry a valid signature by a
  key allowed at that point; those that fail are quarantined (hidden from every reader and
  reported by `mentisdb chain access verify`). An operator-signed checkpoint lets later opens
  verify only what is new, and a chain that no longer matches its checkpoint (truncated or
  rewritten) is locked instead of being served as public. Nothing is served before
  verification finishes.
- **Provenance on every hit.** Every thought JSON carries `provenance`: segment, whether it is
  signed, `signature` (`valid`/`invalid`/`unsigned`), `verified_agent` and
  `trust: "untrusted_data"`. The skill tells agents to treat memories as data, never as
  instructions.
- **Private data stays put.** Merge and dashboard copy refuse sources with private segments,
  governance records or signed thoughts; restoring a backup that extends a governed chain
  imports it as a separate, renamed chain instead of splicing records in.
- A v2-signed append built on an older head fails with the typed `AccessError::StaleHead`
  (HTTP 400, "fetch the head, sign again") instead of a generic signature error.
- `chains.json` entries gain `has_private` and `public_thought_count`, so listings show
  public counts without opening a chain. The dashboard shows chains with private segments
  only behind a PIN.

Ranked search over a chain with a 10,000-thought private segment takes about 423 µs for a
public reader, against 334 µs for a public chain of the same visible size; chains without
private segments pay one comparison. Wire formats and the threat model are in
[docs/CHAIN_ACCESS.md](https://github.com/CloudLLM-ai/mentisdb/blob/master/docs/CHAIN_ACCESS.md).

## Storage and migrations

### Background, journaled migrations

Chain format upgrades used to run synchronously at startup. They now run in the background
and never block startup: the daemon comes up in about a second (measured: 0.97 s with a
600,000-thought legacy chain, which finished migrating 17 s later while the daemon served).

- A request for a chain that is still migrating gets **HTTP 503** with `Retry-After` and the
  percent done (MCP clients get the same text as a tool error). Every other chain is served
  normally.
- Each chain goes through a durable journal: planned, backed up, staged, committed,
  finalized. The original is hard-linked into `migrations/backup/` first (kept 14 days,
  `MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS`), the upgraded chain is streamed into a staging
  file (flat memory, about 120,000 thoughts/s), verified end to end and swapped in with an
  atomic rename. A kill at any step is resumed or discarded on the next start; a chain
  interrupted three times is quarantined instead of crash-looping.
- A stored hash that does not verify (corruption or tampering), a broken hash link in a
  pre-schema-3 chain, or a migration that would invalidate a v2 signature refuses the
  migration and leaves the chain byte-for-byte untouched. The failing chain is quarantined
  and reported while the others carry on.
- Status: `GET /v1/migrations`, `POST /v1/migrations/retry` and `/skip` (global tokens), the
  `mentisdb_migration_status` MCP tool, a dashboard banner, the TUI and:

```bash
mentisdb migrate status                 # what needs, is in, or finished a migration
mentisdb migrate run --dry-run          # rehearse: check hashes and signatures, write nothing
mentisdb migrate run [--chain KEY]      # migrate now (resumes interrupted runs)
mentisdb migrate verify                 # re-check every hash link of the live chains
mentisdb migrate retry --chain KEY      # clear a quarantine; migrated on the next run
mentisdb migrate skip --chain KEY       # leave a chain alone and unavailable until retried
mentisdb migrate rollback --chain KEY   # restore the backup (stop the daemon first)
mentisdb migrate purge-backups          # delete preserved originals of finished migrations
```

### Other storage changes

- **Embedding outside the write lock.** A server append issues a ticket under the lock,
  embeds with no lock held, then commits the vector under a brief lock, in chain order even
  when concurrent appends finish out of order. Lock hold per append with a 5 ms model: about
  9.5 ms before, about 0.33 ms plus a 0.15 ms commit after. A failed embedding schedules a
  background rebuild instead of wedging the sidecar.
- **One daemon per data directory.** A second daemon on a directory already in use refuses to
  start and names the holder's pid; `restore` and the mutating `migrate` commands take the
  same OS lock (a killed daemon releases it).
- **Chains from March and April 2026 open again.** Builds from that period stamped
  `schema_version = 4` on schema-3 records and hashed them with that value, then wrote a third
  schema-3 layout. Schema 4 is now an accepted historical alias, the third layout has its own
  decoder, and alias records are verified under the rule that wrote them instead of being
  rehashed. Both affected real chains verify and migrate with identical content and head hash.
- An empty stored signature (sent by older clients) now counts as unsigned instead of as an
  invalid signature.
- A crash that left most of the final record on disk no longer makes the chain refuse to
  open, and a failed group commit (full disk, failed `fsync`) no longer duplicates the record
  on shutdown.
- A persisted HNSW graph now carries a digest of the embedding space and its documents, so a
  graph built from other vectors is rebuilt instead of served. Existing graphs are rebuilt
  once, in the background.
- A stale or incomplete vector sidecar is rebuilt in the background instead of on the append
  path; freshness now requires one entry per thought.
- Appends no longer rewrite the agent registry per thought, sidecar compaction scales with
  the corpus (append with a 3,000-thought sidecar 4.6 ms to 2.1 ms), the dedup window
  tokenizes each thought once, and rare-term lexical scans are about 5x faster.
- With `MENTISDB_FSYNC=1`, registry files are synced before their rename.

## Compact search results

Contributed by Austin Faulkner-Pearce in
[#25](https://github.com/CloudLLM-ai/mentisdb/pull/25). Ranked and federated search accept
`compact: true` (MCP `mentisdb_ranked_search` / `mentisdb_federated_search`, REST
`/v1/ranked-search` / `/v1/federated-search`, CLI `mentisdb search --compact` and
`mentisdb search federated --compact`). Each hit becomes its `chain_key`, total `score` and
the thought without `hash`, `prev_hash`, `thought_signature`, `signing_key_id`,
`schema_version`, `agent_name`, `agent_owner` and empty top-level fields; score components,
`matched_terms` and `match_sources` are left out.

What compaction never drops: `provenance`, the version fields (`version_status`,
`successors`, `current_version`, `successor_of`, `collapsed_duplicates`), graph provenance,
relations, concepts, refs and content. Fields are dropped by name, so fields added later are
kept, and the public/private boundary is applied before compaction.

Responses were 23.7% smaller on a copy of real chains (11 chains, 12 searches), 34.2%
smaller on a generated corpus and about 25% smaller in the `search_compact` benchmark, with
no time cost. The default response is unchanged. The skill served to agents says when to
pass it (routine recall, scanning many hits) and when not (verifying hashes or signatures,
auditing provenance, debugging scores; `mentisdb_get_thought` returns the full record).

## One host policy for dashboard, REST and MCP

MentisDB now derives one `Host` allowlist and applies it to the dashboard, REST, MCP and the
`cloudllm_mcp` 0.4 library: `localhost`, `127.0.0.1`, `[::1]`, `my.mentisdb.com`,
`MENTISDB_BIND_HOST`, `MENTISDB_DASHBOARD_ALLOWED_HOSTS` and, for a wildcard
`MENTISDB_BIND_HOST`, the machine's interface IPs.

- On a loopback bind (the default), MCP refuses any other `Host` with `403` on `POST /`, the
  `GET /` event stream and the legacy `/tools/*` and `/resources/*` routes, for every client.
- On a wildcard bind (Docker, LAN), MCP does no `Host` check for programmatic clients;
  browsers are still checked.
- REST, and MCP with bearer tokens off, refuse a browser request (one carrying `Origin` or
  `Sec-Fetch-Site`) unless its `Host` and `Origin` host are allowed.

The `GET /` event stream is now the library's: same `Host`, bearer and `Origin` checks as
`POST /`, metadata-only events per JSON-RPC method next to MentisDB's `tool_called` events,
and never tool arguments, results or error text.

The dashboard PIN login, and every other dashboard POST from a real browser, was rejected as
cross-origin because the dashboard's own `no-referrer` policy made browsers send
`Origin: null`. The policy is now `same-origin` and the guard accepts `Sec-Fetch-Site:
same-origin`; cross-site pages are still refused.

## Recall and tooling

- **Standing context.** Startup used to load only the last 12 thoughts, so a preference stated
  months ago was never loaded. `mentisdb_standing_context` (REST `POST /v1/standing-context`,
  CLI `mentisdb thought standing`) returns the durable half of memory regardless of age: live
  constraints, preferences, user traits, relationship updates, goals, decisions and lessons,
  ranked by importance then recency with near-duplicates collapsed, plus the newest handoff
  and checkpoint. The skill loads it before recent context and adds RECALL TRIGGERS: search
  before answering about past preferences, decisions and constraints.
- **The CLI covers everything.** New command groups `thought`, `search`, `chain`, `agent`,
  `entity-type`, `memory`, `skill`, `webhook`, `vectors`, `daemon`, `key` and `migrate` reach
  every MCP tool, REST route and dashboard action, with typed arguments, `--json` output and
  `--help` examples. `docs/CLI_PARITY.md` maps each one, and a test keeps it in sync.
- **pymentisdb** (in the repository's `pymentisdb/` package): compact `ranked_search`, a new
  `federated_search`, the ranking knobs, `provenance` and version fields on thoughts, and
  `ReadSigner` for private chains (`pymentisdb[signing]`).
- `rest_router`, the public way to embed the REST API, now serves the same routes as the
  daemon; chain keys are validated at every entry point (`mentisdb::validate_chain_key`).
- `Cargo.toml` declares `rust-version = "1.90"`, checked in CI.
- The [whitepaper](https://github.com/CloudLLM-ai/mentisdb/blob/master/WHITEPAPER.md) was
  rewritten around agent-memory requirements and checked against the code.

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.11.0.56`.

- **First start may migrate.** If a chain needs a format upgrade, the daemon starts anyway
  and migrates in the background. Until a chain is done, requests for it get `503` with
  `Retry-After`; watch `mentisdb migrate status` or the dashboard banner. Originals are kept
  under `<MENTISDB_DIR>/migrations/backup/` for 14 days. HNSW graph files are rebuilt once in
  the background. Current-format chains are not touched.
- **Public IP or domain? Set `MENTISDB_DASHBOARD_ALLOWED_HOSTS`.** If you reach the daemon by a
  name or address other than the built-in ones (for example a VPS behind a reverse proxy on a
  loopback bind), list it there, in the environment or `.env`, before the daemon starts.
  Otherwise MCP answers `403 Forbidden host` and browsers are refused by REST.
- **One daemon per data directory.** A second daemon (or a second container on the same
  volume) on a directory in use now refuses to start.
- **Key and agent administration moved to the operator.** Clients that registered keys,
  revoked keys, disabled agents or set access policies over REST or MCP must use the
  dashboard or the `mentisdb` operator commands; those REST routes answer `403` and the MCP
  tools are gone. Library callers of `MentisDb::add_agent_key` must pass a
  `KeyRegistrationProof`.
- **Signed appends are verified.** A thought that carries a signature that does not verify
  against an active key of its agent is refused with `403`.
- **Search results change.** Superseded hits are replaced by their successors, near-duplicates
  collapse, and vector fusion is relative; result lists and `total_candidates` differ from
  0.10. Opt out per request (`resolve_successors: false`, `collapse_duplicates: 0`,
  `vector_fusion: "absolute"`, `synonym_weight: 0`) or process-wide
  (`MENTISDB_VECTOR_FUSION=absolute`, `MENTISDB_COLLAPSE_DUPLICATES=off`). Library users get
  the same defaults from `RankedSearchQuery::new()`.
- **Event stream.** `GET /` no longer carries tool arguments, results or error text, and needs
  a bearer token when bearer enforcement is on.
- **Deprecated APIs.** `llm_rerank` and friends and `search::ppr` are no-ops and will be
  removed in the next breaking release.
- **Restores into governed chains.** A backup that extends a signed or allowlist chain is
  imported as a separate, renamed chain instead of fast-forwarding it.
- **Compatibility.** A chain that contains an access-policy record cannot be read by releases
  before 0.11. Other chains remain readable by 0.10.10.55.

## Benchmarks

Full LoCoMo-10P and LongMemEval protocol, fresh ingest, `fastembed-minilm`, the same scripts
and settings as 0.10.9.54 (raw outputs in `results/benchmarks/2026-11-01-full-protocol/`):

| Benchmark | Mode | R@5 | R@10 | R@20 | q/s |
|---|---|---|---|---|---|
| LoCoMo-10P | default | 67.9 | 76.7 | 83.4 | 428 |
| LoCoMo-10P | rerank | 68.6 | 76.9 | 83.5 | 429 |
| LongMemEval | default | 69.8 | 75.6 | 81.4 | 371 |
| LongMemEval | rerank | 70.2 | 75.8 | 82.0 | 362 |

LoCoMo single-hop / multi-hop R@10 (default): 80.4 / 62.9. For comparison, 0.10.9.54 (the
last published run): LoCoMo-10P default R@5/R@10 63.9/73.5, rerank 66.0/74.1; LongMemEval
default R@5/R@10/R@20 68.0/72.8/78.8, rerank R@5 68.6.

## Credits

- Austin Faulkner-Pearce ([@faulknerpearce](https://github.com/faulknerpearce)) for compact
  search results ([#25](https://github.com/CloudLLM-ai/mentisdb/pull/25)).
