# MentisDB 0.12.1.58 (Lean search for agents, superseded memories no longer starve results)

**Date:** November 7, 2026

A focused follow-up to 0.12.0.57, prompted by a user whose chain has held more than 3,500
thoughts since April. Their agent's searches were filling its context window. This release
makes search results cost far fewer tokens, and fixes the bug that made them worse: a
fact rewritten many times could take up a whole page of results. On a generated
3,500-thought chain, a default 10-hit ranked search over MCP went from 23,657 to 5,948
characters, and standing context from 36,191 to 13,050.

There is no data migration and no library break. MCP callers get lean output by default;
see [Upgrading](#upgrading) for what changed on the wire.

## Highlights

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
  chain.

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
  plain case.
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

| Benchmark | Metric | 0.12.0.57 | 0.12.1.58 |
|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 | TBD |
| LoCoMo-10P | rerank R@10 | 76.9 | TBD |
| LoCoMo-10P | default R@5 | 67.9 | TBD |
| LongMemEval | default R@5 | 69.8 | TBD |
| LongMemEval | rerank R@5 | 70.2 | TBD |
