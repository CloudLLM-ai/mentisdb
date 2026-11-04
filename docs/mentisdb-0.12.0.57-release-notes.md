# MentisDB 0.12.0.57 (Shared vectors, derived-state backlog, large signed requests)

**Date:** November 3, 2026

A focused follow-up to 0.11.0.56. It removes the two memory trade-offs that release
documented for large chains, shows operators how far derived state is behind, and
lifts the 1 MiB limit on signed requests behind a body-admission layer that checks the
caller before reading a byte. The minor version changes because two public library types
changed; the proof format and the on-disk format did not.

## Highlights

- **No second copy of every vector.** The background implicit-edge scanner now shares the
  sidecar's vectors instead of copying them, and its queue holds positions instead of
  vectors. At 100,000 thoughts, heap growth while appending fell from 239.2 MiB to
  56.3 MiB.
- **`derived_backlog` in the chain listing** shows embeddings in flight, parked or missing
  and the implicit-edge backlog for each chain.
- **Large imports, admitted safely.** A read proof can declare its body digest in
  `X-Mentis-Content-Sha256`, so a large `mentisdb memory import` with a read key works.
  Bodies are admitted before they are read. The token is checked first, ordinary routes
  take at most 2 MiB, and only the three import routes take up to `MENTISDB_MAX_BODY_BYTES`
  (64 MiB), and only from an authenticated caller. Imports and ordinary requests draw on
  separate budgets.
- **BREAKING for library users:** `VectorSidecarEntry::vector` is now `Arc<[f32]>`, and
  `MentisDbServiceConfig` is `#[non_exhaustive]`.

## Shared vectors and a bounded edge queue

0.11.0.56 moved the implicit-edge neighbour scan off the chain's write lock on chains above
4,096 entries, with two documented trade-offs: the scanner kept its own copy of every
vector (about 150 MB at 100,000 × 384 dimensions), and its queue had no cap. Both are gone.

- `VectorSidecarEntry::vector` is reference-counted (`Arc<[f32]>`). The scanner holds
  pointers to the sidecar's vectors, about 40 bytes per vector instead of 1,536 at 384
  dimensions. Cloning an entry or a whole sidecar now copies pointers, not floats.
- The edge queue stores runs of positions and reads the vectors when a scan starts, so a
  backlog costs a few bytes however long it grows. It still never blocks an append and
  never drops an edge.

Heap growth over the append phase (counting allocator in the `embed_commit_latency`
bench, run at background priority):

| Chain size | Appends | 0.11.0.56 | 0.12.0.57 |
|---|---|---|---|
| 20,000 thoughts | 2,700 | 46.4 MiB | 11.9 MiB |
| 100,000 thoughts | 12,700 | 239.2 MiB | 56.3 MiB |

Peak process RSS after the append phase at 100,000 × 384, measured with the same bench,
went from about 1,120 MiB to 970 MiB. Lock hold is unchanged:
the write-lock hold per append at 100,000 × 384 is 0.028 / 0.60 / 4.3 ms (p50 / p99 / max).

The release verification run on the final 0.12 code also measured 143 MiB less heap at
100,000 thoughts, and exact vector search 4.3x faster (65 ms to 14 ms over 100,000 vectors).

The scan's sustainable rate is about 155 appends per second at 100,000 thoughts and about
720 per second at 20,000 (4 threads, 384 dimensions, a shared machine). If appends outrun
it, nothing is blocked or lost: edges for the newest thoughts appear late, and the backlog
drains when appends slow down (12,475 queued at 100,000 thoughts drained in 80 s). The new
`derived_backlog` shows when that happens.

The sidecar file format and its integrity digest are unchanged: the field serializes
exactly as the `Vec<f32>` did.

## `derived_backlog` in the chain listing

`GET /v1/chains` (global token) and the MCP chain listing (`mentisdb_list_chains`) report,
per chain, the derived work that has not caught up with the chain yet:

```json
"derived_backlog": {
  "embeddings_in_flight": 0,
  "embeddings_parked": 0,
  "embeddings_missing": 0,
  "implicit_edges": 312
}
```

| Field | Meaning |
|---|---|
| `embeddings_in_flight` | thoughts whose embedding was requested but not committed |
| `embeddings_parked` | vectors that are ready but wait for an earlier thought's vector (vectors apply in chain order) |
| `embeddings_missing` | thoughts waiting to be embedded again after a failure |
| `implicit_edges` | applied vectors whose implicit `RelatedTo` edges have not reached the overlay |

All four are zero on a healthy, idle chain. Growing `embeddings_in_flight` or
`embeddings_missing` points at a stalling embedding provider. Growing `implicit_edges`
means appends are outrunning the edge scan. A chain that is not open, or whose private
segments are hidden from the caller, reports zeros. Library: `MentisDb::derived_backlog`
returns a `DerivedBacklog`.

## Large signed requests and body admission

0.11.0.56 documented a limitation: requests signed with `X-Mentis-Auth`, and REST requests
with a bearer token, were buffered up to 1 MiB, so a large `mentisdb memory import` with a
read key got `413`. 0.12 lifts that limit for imports, and in the same step decides how
much of a body the daemon will accept, from whom, before it reads any of it.

### Read proofs that declare their digest

A read proof already signs the SHA-256 of the body. A client that also sends that digest in
`X-Mentis-Content-Sha256: <64 lowercase hex>` lets the daemon verify the proof *before* it
reads the body, and then hash the body as it streams. The signed body goes through. Any
other body fails at its end with `400` before any handler has acted, so nothing is
imported or changed. Without the header, a read proof behaves exactly as before: the
daemon reads up to 1 MiB to check the signature, and its `413` now says how to send more.

The proof format is unchanged, so mentischat and older clients need no update, and a
daemon that predates the header ignores it. The `mentisdb` CLI (`memory import` and every
other command with a read key) and pymentisdb 0.10.1 send the header with every proof:

```bash
mentisdb memory import MEMORY.md --chain vault \
  --read-key-file builder.key --read-agent builder --read-key-id builder-1
```

If you sign requests yourself, add the header and hash exactly the bytes you send.
Library: `read_auth::{body_digest, content_digest_header_value, parse_content_digest,
verify_request_for_digest, CONTENT_DIGEST_HEADER}`, plus a new `content_digest` fuzz
target.

### Body admission: who may send how much

A new outermost layer decides whether to read a body at all:

- **Token first.** With bearer tokens on, the token is checked from the headers. A
  request without a valid token is refused with `401` (or `413` if it declares more than
  2 MiB) without a byte of its body being read.
- **Route caps.** Ordinary routes take at most **2 MiB**. Only the import routes
  (`/v1/import-markdown`, `/v1/skills/upload`, `/v1/extract-memories`) take up to
  `MENTISDB_MAX_BODY_BYTES` (default 64 MiB, allowed range 1 MiB to 1 GiB), and only from
  an authenticated caller. The cap also holds for chunked and HTTP/2 bodies with no
  declared length. Over the cap: `413`.
- **MCP bodies are always capped at 2 MiB,** including the
  `mentisdb_import_memory_markdown` tool. Send a large import to the REST route, or use the
  CLI.
- **Separate budgets.** Large imports draw on their own pool,
  `MENTISDB_MAX_INFLIGHT_BODY_BYTES` (default 4 × the cap). Each import is reserved from
  its declared length, and the reservation is returned as soon as the body has been read.
  Every other request is charged only for the bytes that actually arrive, from a separate,
  fixed 64 MiB pool, so imports never starve appends and searches.
- **Per-peer limits.** One peer may hold at most twice the cap and four large bodies at
  once. An anonymous caller (bearer on, no valid token) may hold at most **8 MiB** of the
  ordinary pool. On a loopback bind every local agent counts as one peer, so the ordinary
  pool has no per-peer limit.
- **Deadlines.** A body that delivers nothing for `MENTISDB_BODY_IDLE_TIMEOUT_SECS`
  (default 30, range 1 to 3600) fails with **`408`**. Small and anonymous bodies also get a
  total deadline of twice that, so a body dripped one byte at a time cannot hold its
  charge.
- **Over budget** is **`503` with `Retry-After`**, also through the bearer-token path.
  Before this, stalled and over-budget bodies were a generic `400`.
- **Bad settings stop the daemon.** A non-numeric or out-of-range
  `MENTISDB_MAX_BODY_BYTES`, `MENTISDB_MAX_INFLIGHT_BODY_BYTES` or
  `MENTISDB_BODY_IDLE_TIMEOUT_SECS` stops the daemon at startup instead of silently
  falling back to a default. An in-flight budget below one maximum body is raised to one
  body.

Read proofs are verified per chain, and the chain is named in the body, so a proof from an
unregistered key cannot be told apart from a real one before the body is read. Proof
requests are therefore bounded by the budgets above rather than by a smaller cap.
Requests that carry a read proof never follow HTTP redirects, in the CLI and now in
pymentisdb too.

## Benchmark harness

`locomo-benches/locomo_bench.py` and `lme-benches/longmemeval_bench.py` gain
`--rebuild-timeout` (default 3600 s) and `--request-timeout` (default 60 s). The vector
rebuild used to have a fixed 600 s client timeout, so on slow or throttled machines the
run continued into read timeouts against a daemon that was still embedding. Now a rebuild
timeout, a dropped connection, or a failed ingest or evaluation request prints
`PHASE FAILED: <phase>` with a hint and exits with status 2:

```bash
python3 lme-benches/longmemeval_bench.py --rebuild-vectors --rebuild-timeout 7200 --request-timeout 120
```

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.12.0.57`.
Python clients: `pip install -U 'pymentisdb[signing]'` (0.10.1). There is no data
migration.

- **Request bodies.** Ordinary REST and MCP routes take at most 2 MiB. Only the three import routes take
  more, up to `MENTISDB_MAX_BODY_BYTES`, and only from an authenticated caller. Large imports
  over MCP fail with `413`: use the REST route or the CLI.
- **New statuses.** A stalled body gets `408`, and an exhausted body budget gets `503` with
  `Retry-After`. Retry the `503`.
- **Reverse proxies** in front of MentisDB must allow request bodies up to
  `MENTISDB_MAX_BODY_BYTES` on the import routes (nginx's `client_max_body_size` defaults to
  1 MiB). Otherwise large imports fail at the proxy.
- **Body settings are validated.** `MENTISDB_MAX_BODY_BYTES` must be 1048576 to
  1073741824, and `MENTISDB_BODY_IDLE_TIMEOUT_SECS` 1 to 3600. Any other value stops the
  daemon at startup.
- **`derived_backlog`** is a new field in each `GET /v1/chains` chain summary.
- **`mentisdb --help`** no longer lists "Supported agents": any agent works over MCP (HTTP
  or stdio), REST or the CLI. The agents `mentisdb setup` can configure are listed in
  `mentisdb setup --help`.

### Library users: `VectorSidecarEntry::vector` is `Arc<[f32]>`

If your code reads or builds `mentisdb::search::VectorSidecarEntry::vector` directly, it
may need a small change. Reading it as a slice works as before. Where you need a `Vec<f32>`,
call `.to_vec()`. `VectorSidecarEntry::new` accepts a `Vec<f32>` or an `Arc<[f32]>`.

Before (0.11):

```rust
use mentisdb::search::VectorSidecarEntry;

let entry = VectorSidecarEntry::new(id, index, hash, vec![0.1, 0.2, 0.3]);
let owned: Vec<f32> = entry.vector.clone();
let literal = VectorSidecarEntry { thought_id: id, thought_index: index,
    thought_hash: hash.to_string(), vector: vec![0.1, 0.2, 0.3] };
```

After (0.12):

```rust
use mentisdb::search::VectorSidecarEntry;
use std::sync::Arc;

let entry = VectorSidecarEntry::new(id, index, hash, vec![0.1, 0.2, 0.3]); // unchanged
let owned: Vec<f32> = entry.vector.to_vec();
let literal = VectorSidecarEntry { thought_id: id, thought_index: index,
    thought_hash: hash.to_string(), vector: Arc::from(vec![0.1, 0.2, 0.3]) };
let slice: &[f32] = &entry.vector; // reading as a slice is unchanged
```

`entry.vector.clone()` still compiles, but it now copies a pointer and returns an
`Arc<[f32]>`, not a `Vec<f32>`.

### Library users: `MentisDbServiceConfig` is `#[non_exhaustive]`

It can no longer be built with a struct literal, so that body-admission settings could be
added without another break. Build it with `MentisDbServiceConfig::new(..)` and the `with_*`
builders. The fields stay readable and assignable.

Before (0.11):

```rust
let config = MentisDbServiceConfig {
    chain_dir: "/var/lib/mentisdb".into(),
    default_chain_key: "project".into(),
    default_storage_adapter: StorageAdapterKind::Binary,
    verbose: true,
    // ...every other field...
};
```

After (0.12):

```rust
use mentisdb::server::MentisDbServiceConfig;
use mentisdb::StorageAdapterKind;
use std::time::Duration;

let config = MentisDbServiceConfig::new("/var/lib/mentisdb".into(), "project", StorageAdapterKind::Binary)
    .with_verbose(true)
    .with_max_body_bytes(128 * 1024 * 1024)       // import routes; clamped to 1 MiB..1 GiB
    .with_max_inflight_body_bytes(512 * 1024 * 1024)
    .with_body_idle_timeout(Duration::from_secs(60));
```

## Benchmarks

Retrieval and scoring did not change since 0.11.0.56. The release verification checked
that with five benchmark runs on the 0.12 code: recall was identical to
[0.11.0.56](mentisdb-0.11.0.56-release-notes.md#benchmarks) on all five, and the LoCoMo
result JSON was byte-identical.

| Benchmark | Metric | 0.11.0.56 | 0.12.0.57 |
|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 | 76.7 |
| LoCoMo-10P | rerank R@10 | 76.9 | 76.9 |
| LoCoMo-10P | default R@5 | 67.9 | 67.9 |
| LongMemEval | rerank R@5 | 70.2 | 70.2 |
| LongMemEval | default R@5 | 69.8 | 69.8 |

With the harness's new configurable timeouts (see [Benchmark harness](#benchmark-harness)),
LongMemEval can now also be run to completion on throttled or slow machines.
