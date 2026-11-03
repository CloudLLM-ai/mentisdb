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
  disable agents or change an agent's status, and a thought's signature was stored without
  being checked. Key and agent administration is now operator-only (dashboard PIN or local
  CLI), keys must be valid Ed25519 points, and a thought that carries a signature must
  verify against an active key of its agent or the append is refused. Thoughts stored
  earlier are checked when they are returned: `provenance.signature` says `valid`,
  `invalid` or `unsigned`, and `verified_agent` is set only for a valid signature by an
  active key; `mentisdb chain access verify` checks a whole chain. A signature is only as
  trustworthy as the registered key that verifies it, so after upgrading review the
  registered keys (dashboard, or `mentisdb agent registry`) and revoke any you do not
  recognize with `mentisdb agent key revoke`.
- **Your dashboard is reachable from other machines.** The PIN could be guessed without
  limit through the `Authorization: Bearer` path (only the login form was rate limited).
  Both now share one limiter: 5 failures per 5 minutes per client (an IPv6 `/64` counts
  as one client), plus a global budget with backoff. If your dashboard was
  exposed, change `MENTISDB_DASHBOARD_PIN`. Dashboard responses now also carry a
  Content-Security-Policy, `X-Frame-Options: DENY` and `nosniff`, which closes
  clickjacking of the state-changing UI.
- **Other local users share the host.** The data directory, chain files and registries,
  including the bearer-token registry (`bearer-tokens.json`: token aliases, scopes and
  hashes) and the webhook registry (signing secrets), were created readable by other
  users under a default umask. New files are created `0600` and new directories `0700`,
  but existing ones keep their mode. Run `chmod 700 <MENTISDB_DIR>` and `chmod 600` on
  the files there now.
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

The pre-release review found and fixed more issues, among them forged standing-context
entries, replacements by unauthorized writers on governed chains, a flood of the read-proof
replay cache, and private-chain counts in listings. See
[Security and reliability fixes from the release review](#security-and-reliability-fixes-from-the-release-review).

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
- On a chain with a signing policy, only the target's own agent or an authorized key can
  replace a thought. Anyone else's link only contests it. On open chains, a replacement by
  another agent is flagged with `cross_agent`. See
  [Who may replace a memory](#who-may-replace-a-memory).

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
`cloudllm_mcp` 0.4 library. The allowlist contains:

- `localhost`, `127.0.0.1` and `[::1]`;
- `MENTISDB_BIND_HOST` and `MENTISDB_DASHBOARD_ALLOWED_HOSTS`;
- for a wildcard `MENTISDB_BIND_HOST`, the machine's interface IPs;
- on the TLS listeners only, `my.mentisdb.com`.

How each bind applies it:

- **Loopback bind (the default):** MCP and REST refuse any other `Host` with `403` for every
  client. On MCP that covers `POST /`, the `GET /` event stream and the legacy `/tools/*` and
  `/resources/*` routes.
- **Wildcard bind (Docker, LAN):** there is no `Host` check for programmatic clients.
- **Browser requests** (ones carrying `Origin` or `Sec-Fetch-Site`) are refused on any bind
  unless their `Host` and `Origin` host are allowed.

The `GET /` event stream is now the library's: `Host` and `Origin` checks as on `POST /`, a
global bearer token when tokens are on, metadata-only events per JSON-RPC method next to
MentisDB's `tool_called` events, and never tool arguments, results or error text.

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
- **pymentisdb 0.10.0 on PyPI** (`pip install 'pymentisdb[signing]'`): compact
  `ranked_search`, a new `federated_search`, the ranking knobs, `provenance` and version
  fields on thoughts, and `ReadSigner` for private chains. It also fixes the package
  itself: the 0.9.0 wheel contained no code, and `import pymentisdb` failed without
  LangChain or on langchain-core 1.x. LangChain is now optional (`pymentisdb[langchain]`),
  and `MentisDbMemory` works on langchain-core 1.x through `langchain-classic`.
- `rest_router`, the public way to embed the REST API, now serves the same routes as the
  daemon; chain keys are validated at every entry point (`mentisdb::validate_chain_key`).
- `Cargo.toml` declares `rust-version = "1.90"`, checked in CI.
- The [whitepaper](https://github.com/CloudLLM-ai/mentisdb/blob/master/WHITEPAPER.md) was
  rewritten around agent-memory requirements and checked against the code.

## Security and reliability fixes from the release review

Before tagging, the release went through an independent security review and a
correctness and performance review. Everything they found is fixed in 0.11.0.56. Several
of these fixes change behaviour; those changes are repeated under [Upgrading](#upgrading).

### Who may replace a memory

Valid-version retrieval lets a newer thought take an older one's place in search and
standing context. The review tightened who may do that:

- **Governed chains.** On a chain with a signing policy, a `Supersedes`, `Corrects` or
  `Invalidates` link replaces its target only if the replacing thought was verified when
  the chain opened and was written by the same agent or by a key authorized on the chain.
  Any other link only *contests* the target: it stays current with
  `version_status: "contested"`, and the link is listed under `successors` with
  `authorized: false`. Single-chain replacements are judged against the chain's current
  policy.
- **Open chains.** Corrections from another agent still redirect, because that is the
  point of the feature, but now you can see them. A successor note carries `agent_id`,
  `trust`, `cross_agent` and `authorized`. Standing context prints
  `[REPLACES #n ... replaced by another agent, check it]`, and the skill tells agents to
  check those entries.
- **Cross-chain and branch lineage** use the same successor notes and the same
  authorization as single-chain lineage, plus the `chain_key` the successor lives on
  (`SuccessorNote.chain_key`).
- **Federated and branch search no longer deadlock.** Resolving cross-chain lineage held
  a read lock on every searched chain at once. Two searches over the same chains in
  opposite order, with writers queued behind them, could block those chains until a
  restart. Lineage now reads one chain at a time.
- **Depth cap.** A write that would make a chain of more than 64 versions of one thought
  is refused, and reads follow supersessions only that far.

### Standing context and prompts

Stored text could forge the standing-context block an agent loads at session start. For
example, a thought containing line breaks could close the block early and add a fake
`SYSTEM:` line or a fake top-ranked constraint.

- Every entry is now rendered as a single line. Line breaks, control characters and
  invisible characters (bidi overrides, zero-width and tag characters) are shown as
  visible marks, and `===` and `[#` are broken up. The same escaping covers the catch-up
  and bootstrap prompts and the `MEMORY.md` export.
- Each entry shows `trust=verified|unsigned|unverified`, and the block opens by saying
  its entries are data, not instructions.
- On a chain that requires signatures, an entry from an unverified writer ranks with
  importance capped at 0.5. On an open chain, importance is used as written.

### Private chains

- **Read-proof replay cache.** The cache of spent `X-Mentis-Auth` nonces could be flooded
  with self-signed proofs from throwaway keys, locking honest readers out. A nonce is now
  spent only when a proof is about to grant access to a private segment. The cache is
  capped at 25,000 entries per key and 100,000 in total. A proof refused because the
  cache is full now reads the public view instead of getting `503`. Malformed, stale,
  badly signed and already-spent proofs still get `401`.
- **Chain listings.** `GET /v1/chains` and the MCP list tool showed full thought and agent
  counts and the storage path for a private chain that could not be opened. They now
  show only the public count, no agents and an empty path. A chain the registry has never
  seen is treated as private until it is first opened.
- **Dashboard routes without a PIN.** On a daemon that holds any private chain and has no
  dashboard PIN, deleting a chain, writing settings, creating, revoking or deleting
  bearer tokens, and restarting the daemon now answer `403`.
- **Probing private targets.** A writer who may not read private segments got a different
  error when linking to a private thought than when linking to a missing one, which
  revealed which thoughts were private. Both now fail with the same generic refusal.

### Dashboard, network and keys

- **PIN limiter.** Failed attempts are now counted per IPv6 `/64` (or per IPv4 address),
  the table is capped at 4,096 clients, and a global budget applies: more than 50
  failures from all clients together in 5 minutes lock every PIN attempt for 60 seconds,
  doubling on each repeat up to one hour. The lock is forgiven after two quiet hours, and
  `429` responses carry `Retry-After`. A signed-in session is unaffected. Behind NAT or a
  reverse proxy, all clients share one bucket. Use a long random PIN (12 or more
  characters) for any dashboard reachable beyond localhost.
- **Dashboard login.** Real browsers could not log in to a PIN-protected dashboard (fixed
  in b55185d). The dashboard's own `Referrer-Policy: no-referrer` made browsers send
  `Origin: null`, which the CSRF guard rejected. The policy is now `same-origin`, and the
  guard accepts `Sec-Fetch-Site: same-origin`.
- **REST `Host` checks.** On a loopback bind, REST now checks the `Host` header on every
  request, not only on browser requests, as MCP already does. Requests with no `Host` and
  wildcard or LAN binds are unchanged. A reverse proxy in front of a loopback daemon must
  forward an allowed name, as described under [Upgrading](#upgrading).
- **`my.mentisdb.com` on TLS only.** It is a public DNS name, so it is now accepted as
  `Host` and `Origin` only on the TLS listeners (HTTPS MCP 9473, HTTPS REST 9474, dashboard
  9475). The plain HTTP ports refuse it unless you list it in
  `MENTISDB_DASHBOARD_ALLOWED_HOSTS`.
- **Event stream.** `GET /` reports every tool call on every chain, so it now needs a
  global token. MCP methods the authorization table does not name also need a global
  token. Event fields are validated.
- **Exposure warning.** A listener reachable from other machines without bearer tokens,
  or a dashboard on a non-loopback address without a PIN, now produces a `SECURITY:`
  warning in the log, on stderr (so it shows in `docker logs`), in the TUI and as a red
  dashboard banner.
- **`mentisdb key generate`** no longer prints the private seed, where it could end up in
  scrollback, shell logs or an agent's context. It always writes the seed to an
  owner-only file and prints only the public key and the path:
  ```bash
  mentisdb key generate --out builder.key   # or $MENTISDB_KEY_DIR, default ~/.mentisdb/keys/
  ```
  Seeds now come from the operating system's random number generator. Loading a seed
  file that other users can read still works but prints a `chmod 600` warning.
- **File permissions.** Directories MentisDB creates are `0700`. Chain files, registries
  (including the webhook registry, which holds signing secrets), sidecars, journals and
  backup archives are `0600`. Existing files keep their mode, and the daemon warns about
  a data directory that others can read. Backups still leave out
  `operator/operator.key`, and `backup` and `restore` now remind you to back it up
  separately.
- **The CLI and stdio proxy no longer follow redirects,** which would have re-sent read
  proofs and bearer tokens to the redirect target. A `3xx` is now an error that asks you
  to point `--url` at the final address.
- **TLS certificates are checked at startup.** An expired certificate that MentisDB
  generated is regenerated with the same names and a new key, with a warning that
  clients must trust the new one. A certificate that expires within 30 days produces a
  warning naming `mentisdb cert --force`. An expired certificate you supplied yourself
  is only warned about.
- **CI** workflows are read-only except a tag-only release job, and third-party actions
  are pinned to commit hashes.

### Limits on caller-sized requests

Very large windows could make a single search take minutes. Requests above these limits
now get `400` with a message naming the maximum, on REST, MCP and the CLI:

| Request | Limit |
|---|---|
| `offset + limit` on ranked, lexical and federated search and context bundles | 10,000 |
| `per_type_limit` on standing context | 100 |

The window is refused, not clamped, because a clamped page would be the wrong page. To
read deeper, narrow the query with filters or `since`/`until`.

### Migrations

- **A clean shutdown no longer counts toward quarantine.** Before, three ordinary
  restarts (`SIGTERM`, `systemctl restart`, quitting the TUI) during a long migration
  quarantined the chain. A clean shutdown now hands its attempt back, and the daemon
  waits up to 15 seconds for the migration to stop. Only a crash counts.
- `migrate retry` through a running daemon changes the chain's status in one step, so no
  request can read a chain that is about to be rewritten.
- The recovery commands in error messages (the `503` body, `migrate status` and others)
  now name the flag the CLI requires: `mentisdb migrate retry --chain <key>`.
- A huge retention value (`--older-than-days`, `MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS`)
  used to overflow to zero and treat every backup as expired. It now saturates, so a very
  long retention keeps everything.

### Write path and storage

- A failed index flush after a thought was already saved no longer turns the append into
  a `500`. Clients that retried those errors wrote the thought twice.
- One slow embedding no longer triggers re-embedding the whole corpus. Later vectors wait
  behind it (up to 512), and only the failed thought is embedded again. At 20,000
  thoughts, a 600-append straggler used to re-embed 20,667 thoughts with a 5.1 s stall;
  now it embeds 601, and the worst stall is 0.29 s.
- Vector sidecar compaction runs on a background thread instead of under the write lock,
  and loading a sidecar while a compaction swaps its files no longer loses the WAL.
  Malformed WAL rows are rejected instead of loaded.
- Above 4,096 entries, the implicit-edge neighbour scan runs off the write lock. At
  100,000 thoughts × 384 dimensions, write-lock hold per append (p50 / p99 / max) went
  from 24.1 / 26.3 / 38.6 ms to 0.03 / 0.72 / 16.3 ms, and the worst stall when adopting a
  rebuilt index went from 1,464 ms to 0.7 ms. The trade-offs:
  - About 150 MB of duplicate vectors in memory at 100,000 × 384.
  - Implicit edges can trail the chain by a few appends
    (`MentisDb::implicit_edge_backlog`).
  - The scan queue has no cap.

### Search

- Near-duplicate collapse is faster with identical results. A 1,000-hit window went from
  322 ms to 5 ms.
- A ranked search's `total` no longer counts superseded thoughts, unless
  `include_invalidated` is set.
- Standing context compares only each type's best candidates, so it stays fast on large
  chains.

### Known limitation

Signed requests (`X-Mentis-Auth`) and REST requests with a bearer token are buffered up
to 1 MiB, so a larger body gets `413`. In practice this hits a large `mentisdb memory
import` run with a read key. Import in batches of under 1 MiB.

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.11.0.56`.
Python clients: `pip install -U 'pymentisdb[signing]'` (0.10.0).

- **First start may migrate.** If a chain needs a format upgrade, the daemon starts anyway
  and migrates in the background. Until a chain is done, requests for it get `503` with
  `Retry-After`; watch `mentisdb migrate status` or the dashboard banner. Originals are kept
  under `<MENTISDB_DIR>/migrations/backup/` for 14 days. HNSW graph files are rebuilt once in
  the background. Current-format chains are not touched.
- **Public IP, domain or reverse proxy? Set `MENTISDB_DASHBOARD_ALLOWED_HOSTS`.** If you
  reach the daemon by a name or address other than the built-in ones (for example a VPS
  behind nginx or Caddy on a loopback bind), list the name the proxy forwards as `Host`,
  in the environment or `.env`, before the daemon starts. Alternatively, have the proxy
  send `Host: 127.0.0.1:9472` upstream. Otherwise MCP and REST answer `403`.
- **`my.mentisdb.com` over plain HTTP.** Switch `http://my.mentisdb.com:9471` / `:9472`
  clients to `https://my.mentisdb.com:9473` / `:9474`, or to `http://localhost:9471` /
  `:9472`, or list the alias in `MENTISDB_DASHBOARD_ALLOWED_HOSTS` to keep the old
  behaviour.
- **New request limits.** `offset + limit` above 10,000 on ranked, lexical and federated
  search and context bundles, and a standing-context `per_type_limit` above 100, now get
  `400` on REST, MCP and the CLI instead of a very slow answer.
- **Private chains and a dashboard without a PIN.** If the daemon holds private chains,
  set `MENTISDB_DASHBOARD_PIN` to delete chains, change settings, manage bearer tokens or
  restart from the dashboard. Without a PIN those routes answer `403`.
- **`mentisdb key generate`** no longer prints the seed. Scripts that parsed
  `private_key_seed_hex:` must pass `--out FILE` and read the seed from that file.
- **File permissions.** Existing data directories keep their mode. Run
  `chmod 700 <MENTISDB_DIR>`, and back up `operator/operator.key` separately, because
  backups leave it out.
- **Large signed requests.** Requests signed with `X-Mentis-Auth`, and REST requests with a
  bearer token, are limited to 1 MiB (`413` above that). Import big `MEMORY.md` files in
  batches.
- **One daemon per data directory.** A second daemon (or a second container on the same
  volume) on a directory in use now refuses to start.
- **Key and agent administration moved to the operator.** Clients that registered keys,
  revoked keys, disabled agents or set access policies over REST or MCP must use the
  dashboard or the `mentisdb` operator commands; those REST routes answer `403` and the MCP
  tools are gone. Library callers of `MentisDb::add_agent_key` must pass a
  `KeyRegistrationProof`.
- **Signed appends are verified.** A thought that carries a signature that does not verify
  against an active key of its agent is refused with `403`.
- **Search results change.** Superseded hits are replaced by their successors (on governed
  chains only when the replacement is authorized), near-duplicates collapse, and vector
  fusion is relative. Result lists, `total` and `total_candidates` differ from 0.10. Opt out per request (`resolve_successors: false`, `collapse_duplicates: 0`,
  `vector_fusion: "absolute"`, `synonym_weight: 0`) or process-wide
  (`MENTISDB_VECTOR_FUSION=absolute`, `MENTISDB_COLLAPSE_DUPLICATES=off`). Library users get
  the same defaults from `RankedSearchQuery::new()`.
- **Event stream.** `GET /` no longer carries tool arguments, results or error text, and with
  bearer enforcement on it needs a global token. A monitor that subscribes with a
  chain-scoped token must switch to a global one.
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

**The release-review fixes did not move recall.** We ran the same protocol again on
4e6cd3f, after the security and correctness fixes landed (raw outputs in
`results/benchmarks/2026-11-03-0.11.0.56/`). Every figure matches the table above to within
0.2 points, and LoCoMo default answered the same 1,516 of 1,977 questions at R@10, query for
query. A control run on 1d4be8f, which moved the implicit-edge scan off the write lock,
agreed to within one query.

Throughput on that day could not be compared with the table above, because the machine was
busy with other builds. To check for a slowdown, we ran the 0.11 baseline binary, 4e6cd3f,
1d4be8f and the baseline binary again, back to back, under the same macOS background policy
(efficiency cores only). Ingest time and LoCoMo q/s for both release builds fell inside or
above the range of the two baseline runs, so the fixes cost no measurable throughput. The
efficiency-core numbers are in the results README; they are not comparable with the q/s
column above.

## Credits

- Austin Faulkner-Pearce ([@faulknerpearce](https://github.com/faulknerpearce)) for compact
  search results ([#25](https://github.com/CloudLLM-ai/mentisdb/pull/25)).
