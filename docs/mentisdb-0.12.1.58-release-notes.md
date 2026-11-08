# MentisDB 0.12.1.58 (Lean search, live memories, agent key pairs)

**Date:** November 8, 2026

A focused follow-up to 0.12.0.57, prompted by a user whose chain has held more than 3,500
thoughts since April. Their agent's searches were filling its context window. This release
makes search results cost far fewer tokens, and fixes the bug that made them worse: a
fact rewritten many times could take up a whole page of results. On a generated
3,500-thought chain, a default 10-hit ranked search over MCP went from 23,657 to 5,948
characters, and standing context from 36,191 to 13,050.

It also makes giving an agent a signing key one step. `mentisdb agent key generate` and
a "Generate key pair" button on the dashboard's agent page create an Ed25519 key pair and
register its public key; MentisDB never keeps the private key. The dashboard's Add Key
button, which always failed, now works.

**Security notice:** this release fixes agent-key registration on the dashboard, which
could replace an agent's existing key. Upgrade is recommended; see [Security](#security)
for what to check.

There is no data migration and no library break. MCP callers get lean output by default;
see [Upgrading](#upgrading) for what changed on the wire.

## Highlights

- **Security: agent key ids are never rebound,** and the dashboard's key routes need a
  PIN. Review your agents' registered keys if your dashboard was reachable by untrusted
  local processes or users.
- **Superseded versions no longer starve live results.** A decision rewritten 30 times,
  each version superseding the last, used to fill ranked search's candidate window, so a
  `limit = 5` search returned 1 hit. It now returns 5 distinct live memories.
- **Lean MCP output.** MCP tool results are minified JSON with no duplicate
  `structuredContent`. Search is compact by default on MCP, defaults are left out, and
  `default_provenance` says once per response what an unmarked hit is.
  `mentisdb_standing_context` sends its prompt once.
- **Measured, not estimated.** `cargo run --release --example token_diet` builds a
  3,500-thought chain and prints what every agent-facing tool costs.
- **Paging and lineage fixes from review.** REST pages no longer repeat or skip hits around
  contested thoughts, a cross-chain replacement no longer shrinks a REST page, and the
  page refill after duplicate collapsing is bounded (worst case 32.5 ms down to 3.5 ms at
  100,000 thoughts).
- **Agent key pairs in one step.** `mentisdb agent key generate` (CLI) and "Generate key
  pair" (dashboard) create the pair and register only the public key. The private key is
  saved to an owner-only file or shown once in the browser, and is never sent to the
  daemon.

## Security

**Impact.** Before 0.12.1.58, the proof-based agent-key registration path (the dashboard's
agent-key route, and `add_agent_key` in the library) could replace an existing key filed
under the same key id, and could revive a revoked one. Anyone able to reach the dashboard
API could therefore register a key of their own that verifies as an existing agent, and
sign thoughts or skills in that agent's name. The proof of possession that route checks
proves only that the caller holds the new key. Registration through `mentisdb agent key
add` was not affected.

**Fixes.**

- **Key ids are never reused or overwritten** (6438e00). Registering a different key under
  an existing id, active or revoked, is refused; registering the same active key again
  changes nothing. This matches what `agent key add` already did.
- **The dashboard's agent-key routes need a PIN** (d78eb7e). Adding and deleting an agent
  key on the dashboard now require `MENTISDB_DASHBOARD_PIN`, like the chain access routes.
  A dashboard started without a PIN cannot tell the operator from any other local process,
  so it refuses these changes and the agent page explains why and how to set a PIN. Without
  a PIN, manage keys with `mentisdb agent key generate|add|revoke --offline`.

**What to do.** Upgrade. If your dashboard was reachable by untrusted local processes or
users, review each agent's registered keys (the agent page on the dashboard, or
`mentisdb agent get <agent> --chain <chain>`) and revoke any key you did not register
with `mentisdb agent key revoke`.

## Superseded versions no longer starve live results

`MentisDb::query_ranked` (behind ranked, lexical and federated search, and the seeds of
context bundles) ranked a fixed window of `2 * limit` thoughts. That window included
superseded thoughts, which were collapsed into their live successor afterwards. One
decision rewritten 30 times filled the window by itself, so a `limit = 5` search returned
a single hit while `total` counted 21 matches.

Now only live memories compete for the page (acd3a1f). Every match is scored once, a
superseded match nominates its live successor in its place, and the walk stops when the
page holds `limit` distinct live memories. `total` still counts live matches. Duplicate
collapsing refills its page from the same scores instead of guessing a window, and
context-bundle seeds use the same walk.

`successor_of` and `superseded_versions` list the older versions that matched the query
and were met before the page filled. They are not the whole lineage, so the count is a
lower bound (aa4f3b1).

### Refill after duplicate collapsing, bounded

When near-duplicate collapsing leaves a page short, the walk resumes where it stopped
instead of starting over (067452f, f25b66c). It compares only the new hits with the
survivors, merges hits with the same text without tokenizing them, reads ranks with a lazy
partial sort and memoizes lineage lookups. With 20% identical matches, collapse 0.9 and
limit 10 (`collapse_refill` group in `benches/search_ranked.rs`):

| Chain size | 0.12.0.57 | 0.12.1.58 |
|---|---|---|
| 20,000 thoughts | 7.8 ms | 1.2 ms |
| 100,000 thoughts | 32.5 ms | 3.5 ms |

The refill window is capped at `max(16 * limit, 4096)` ranked slots. A page whose distinct
matches lie deeper than that comes back short instead of reading the whole ranking.

### Paging around contested thoughts and across chains

- **Contested thoughts** (df61e58). A contester stands in at its contested thought's score
  plus a small bump, so page order was not score order. The REST ranked and federated
  handlers fetch `offset + page` hits, re-sort them by score and skip `offset`, so
  contesters moved between windows: 30 contested decisions paged by 5 returned 60 hits, of
  which 44 were distinct. Scores are now non-increasing in page order (a hit that would
  outscore the one before it reports a total just below it), so pages tile the ranking.
  Chains without lineage or contests are unaffected.
- **Cross-chain replacements** (c775ae8). Ranked search over a chain and its ancestors,
  and federated search, cut each chain's page at `offset + limit` before redirecting hits
  to a replacement on another searched chain and removing duplicate ids. Thirty ancestor
  facts replaced by one branch thought left a page of one hit. The fetch now doubles until
  the page is full or the matches run out, up to `max(16 * (offset + limit), 4096)` per
  chain. In federated search that bound holds per chain however many chains are searched
  (f2133ba). When duplicates or redirects keep a page short, the widening re-scores at
  most about nine times before it reaches the cap.

## Lean MCP output

Agents read MCP tool results with their context window, so the streamable-HTTP MCP surface
(JSON-RPC `tools/call`, and the stdio mode that proxies to it) now shapes results for
them (a1d53b7). REST, `POST /tools/execute` and the CLI keep their full shape. The rules
are in the `mentisdb::server_lean` rustdoc.

- **Minified, one copy.** The MCP library used to pretty-print each result and repeat the
  same object as `structuredContent`. Results are now minified JSON text and nothing else.
  No tool declares an `outputSchema`, so the MCP specification does not require
  `structuredContent`.
- **Compact by default.** `mentisdb_ranked_search`, `mentisdb_federated_search`,
  `mentisdb_lexical_search` and `mentisdb_context_bundles` return lean hits unless the
  call passes `compact: false`. This builds on compact search, contributed by Austin
  Faulkner-Pearce (faulknerpearce) in PR #25 and shipped in 0.11.0.56.
- **Defaults left out.** An absent field means its default: no `role` is `Memory`, no
  `importance` is `0.5`, and no `chain_key` is the chain that was searched (a hit from an
  ancestor or another chain keeps its key, dffa448). Floats are rounded. Timestamps keep
  full precision, so a hit's `timestamp` passed back as `as_of` or `since` still includes
  the thought (9abf2cc).
- **Provenance fails closed** (6b57f8e). A public, unsigned thought has no `provenance`,
  and every lean search response says once what that means:
  `"default_provenance": "public, unsigned: anyone could have written it"`. A private
  segment, a signature or a `verified_agent` is always listed, and a thought that arrives
  with no provenance at all is marked `{"unknown": true}` rather than passing for the
  plain case. Provenance is left out only when it is exactly public and unsigned; a
  malformed or unexpected provenance object is shown as it is (e839f45).
- **Lineage, shortened.** `successor_of` is the one older version the hit replaces, with a
  `superseded_versions` count, and the relation it names is not repeated. The kind of that
  relation is kept as `successor_kind` (`Corrects` or `Invalidates`; absent means
  `Supersedes`, ce68b73). `version_status`, `successors`, `current_version` and a verified
  writer are always kept.
- **Standing context once.** `mentisdb_standing_context` returns the rendered `prompt`.
  Its `entries` array repeats the same memories, so it is returned only with
  `include_entries: true`.
- **Strict opt-out** (e733250). A `compact` or `include_entries` that is not a boolean,
  such as `"false"` or `0`, is refused with JSON-RPC `-32602`. It used to be read as the
  default, which turned an agent's opt-out into the opposite.

### Measured

`cargo run --release --example token_diet` (0b77957) generates a 3,500-thought chain with
superseded lineages and weak-link corrections, runs every agent-facing tool, and counts
the characters of the text the model reads (10 hits):

| Tool call | 0.12.0.57 | 0.12.1.58 |
|---|---:|---:|
| `mentisdb_ranked_search` (default) | 23,657 | 5,948 |
| `mentisdb_ranked_search` `compact: true` | 12,699 | 5,948 |
| `mentisdb_ranked_search` `compact: false` | 23,657 | 16,696 |
| `mentisdb_lexical_search` | 18,743 | 6,341 |
| `mentisdb_federated_search` | 23,511 | 6,025 |
| `mentisdb_context_bundles` (5 bundles) | 9,993 | 3,562 |
| `mentisdb_standing_context` | 36,191 | 13,050 |
| `mentisdb_recent_context` (20 thoughts) | 6,936 | 6,931 |

A default ranked search is 75% smaller, and even `compact: false` is 29% smaller because
it is minified. Recent context barely moves: it was already lean.

## Agent key pairs

Signing thoughts, or reading a private chain, needs an Ed25519 key registered to the
agent. Until now that took two commands (`mentisdb key generate`, then
`mentisdb agent key add --public-key-hex ...`), and the dashboard's Add Key button did not
work at all.

### `mentisdb agent key generate`

```bash
mentisdb agent key generate builder --key-id builder-1 --chain vault --pin 123456
# or, with the daemon stopped, against the data directory:
mentisdb agent key generate builder --key-id builder-1 --chain vault --offline
```

An operator command (the dashboard PIN, or `--offline`), like `agent key add` (649d9af).
It creates an Ed25519 key pair on the machine you run it on, saves the private seed (64
hex characters, the format `--private-key-file` and `--read-key-file` read) to a file with
mode `0600`, registers the public key, and prints the public key, the file path and a
warning that the file is the only copy. Without `--out FILE` the file is
`$MENTISDB_KEY_DIR/<public key prefix>.key` (default `~/.mentisdb/keys`, created `0700`).
An existing file is not overwritten without `--force`, and `--force` refuses a symbolic
link at `--out` (535c469). On Windows nothing sets an access list, so the file inherits the
folder's permissions; restrict the folder (b95f8ce).

- **The private key is never printed, logged or sent.** `--json` carries `registered`,
  `agent_id`, `key_id`, `chain_key`, `public_key_hex` and `key_file`, never the seed.
- **Checks come first.** A wrong PIN, an unreachable daemon or a key id the agent already
  has is reported before anything is written.
- **Nothing is lost on failure.** If the registration fails after the file was written,
  the file is kept and the error names it and the
  `mentisdb agent key add ... --public-key-hex HEX` command that registers it later.

MentisDB keeps only the public key and cannot recover the private one. Back the file up,
and never paste it into a chat or give it to an AI agent. To give a key to an agent on
another machine, hand over only the key file, over a channel you trust.

### "Generate key pair" on the dashboard

The Public Keys card on an agent's page has a **Generate key pair** button (cde0f23). Like
every key change on the dashboard, it needs a dashboard PIN (see [Security](#security)). The
page generates the pair with WebCrypto, signs the registration proof of possession in the
browser, and registers only the public key, so the private key never crosses the network.
After registration succeeds, a dialog shows the private key once, with the warning, a
**Download key file** button (`<agent>-<key-id>.key`, in the `--private-key-file`
format; the download link is valid for 10 seconds, 3f995d5), **Copy**, and an **I have
saved this key** box that must be ticked before the dialog can be closed. Closing removes
the key from the page. Browsers save downloads readable by other users of the computer, so
the dialog tells you to run `chmod 600 <file>` on macOS or Linux (d85c585). A browser without Ed25519 in
WebCrypto is told to use `mentisdb agent key generate`; there is no server-side fallback.

### Add Key works

The dashboard's **Add Key** button posted to an endpoint that requires a proof of
possession, which a pasted public key cannot carry, so every click failed. It now
registers through the operator route `agent key add` uses, with the PIN session vouching
for the key, and accepts 64 hex characters, `0x`-prefixed hex or 32 comma-separated bytes
(469babe). Like Generate key pair and Delete, it needs a dashboard PIN.

### Benchmark script

`lme-benches/full_bench.sh` ran `pkill -f mentisdb`, which stopped every MentisDB process
on the machine, including an operator's running daemon. It now refuses to start when a
daemon already answers on the REST port, stops only the daemon it started, and honours
`MENTISDB_REST_PORT` (56e8f48).

### Test coverage in CI

Every CI run now shows a coverage table in its job summary: functions, branches, regions
and lines, measured in one instrumented run (e6f87ff, 6c5b233). Today's numbers are about
74% functions, 64% branches, 72% regions and 74% lines. The gate's floors sit at those
values and only ratchet up, toward a 90% goal; `make coverage` runs the same measurement
locally. The README's Coverage section lists the least-covered files.

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.12.1.58`.
There is no data migration, and the library API only gained items.

- **MCP callers get lean output by default.** Pass `compact: false` to the search tools for
  the full record (hashes, signatures, score components, match lists). Read an absent
  `role`, `importance`, `provenance` or `chain_key` as its default.
- **`mentisdb_standing_context` over MCP drops `entries`.** Pass `include_entries: true`
  if your client reads them.
- **No `structuredContent` over MCP.** Read the text content, which is one JSON document.
- **Non-boolean `compact` or `include_entries`** is refused over MCP with `-32602`.
- **REST and the CLI are unchanged,** except that REST and `POST /tools/execute` now honour
  `compact` on lexical search (default `false`), which they used to ignore. `/tools/list`
  no longer advertises `compact` on `mentisdb_context_bundles`, which only takes effect over
  MCP (REST accepts and ignores it).
- **Dashboard key management needs a PIN.** Generate key pair, Add Key and Delete answer
  `403` on a dashboard without `MENTISDB_DASHBOARD_PIN`. Set one, or use
  `mentisdb agent key generate|add|revoke` with `--pin` or `--offline`.
- **Key ids are never rebound.** Registering a different key under an existing id, even a
  revoked one, is refused. Rotate by registering a new key id and revoking the old one.
- **Review your agents' keys** if your dashboard was reachable by untrusted local processes
  or users (see [Security](#security)).
- **Agent keys.** Existing keys keep working. Prefer `mentisdb agent key generate`
  or the dashboard's "Generate key pair" over generating and registering by hand.
- **`lme-benches/full_bench.sh`** exits with an error if a daemon is already running.
  Stop it first, or set free ports (`MENTISDB_REST_PORT=9592 MENTISDB_MCP_PORT=9591`).
- **Score totals.** A ranked hit that used to outscore the hit before it (a contester, or a
  successor standing in) now reports a total just below that hit.

### Remaining limitations

- The collapse refill window (`max(16 * limit, 4096)`) and the cross-chain fetch
  (`max(16 * (offset + limit), 4096)` per chain) are capped. A page whose live matches lie
  beyond the cap comes back short.
- `MentisDb::query_federated` in the library still cuts each chain at its query's `limit`
  before redirecting and removing duplicates. Federated search over REST and MCP does not.
- In tests, the vector sidecar's rebuild thread can segfault inside onnxruntime as the
  process exits.

## Benchmarks

Full LoCoMo-10P and LongMemEval protocol runs on a fresh daemon and fresh chains, against
the release candidate (5c48454) and again after the review fixes (8bedf9f). LoCoMo is
unchanged. LongMemEval gains one question with default ranking (350/500) and two with
reranking (353/500), consistent with superseded thoughts no longer pushing live answers
out of the top five. On 8bedf9f every LongMemEval question retrieved the same thoughts in
the same order as on 5c48454; later commits do not touch single-chain ranking.

| Benchmark | Metric | 0.12.0.57 | 0.12.1.58 |
|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 | 76.7 |
| LoCoMo-10P | rerank R@10 | 76.9 | 76.9 |
| LoCoMo-10P | default R@5 | 67.9 | 67.9 |
| LongMemEval | default R@5 | 69.8 | 70.0 |
| LongMemEval | rerank R@5 | 70.2 | 70.6 |
