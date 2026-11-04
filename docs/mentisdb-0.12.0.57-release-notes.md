# MentisDB 0.12.0.57 (Shared vectors, derived-state backlog, large signed requests)

**Date:** November 3, 2026

A focused follow-up to 0.11.0.56. It removes the two memory trade-offs that release
documented for large chains, shows operators how far derived state is behind, and
lifts the 1 MiB limit on signed requests. The minor version changes because one public
library type changed; the wire protocol and the on-disk format did not.

## Highlights

- **No second copy of every vector.** The background implicit-edge scanner now shares the
  sidecar's vectors instead of copying them, and its queue holds positions instead of
  vectors. At 100,000 thoughts, heap growth while appending fell from 239.2 MiB to
  56.3 MiB.
- **`derived_backlog` in the chain listing** shows embeddings in flight, parked or missing
  and the implicit-edge backlog for each chain.
- **Large signed requests.** A read proof can declare its body digest in
  `X-Mentis-Content-Sha256`, which lifts the 1 MiB limit. Large `mentisdb memory import`
  with a read key works, and `MENTISDB_MAX_BODY_BYTES` (default 64 MiB) sets the
  overall cap.
- **BREAKING for library users:** `VectorSidecarEntry::vector` is now `Arc<[f32]>`.

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

## Large signed requests

0.11.0.56 documented a limitation: requests signed with `X-Mentis-Auth`, and REST requests
with a bearer token, were buffered up to 1 MiB, so a large `mentisdb memory import` with a
read key got `413`. That limit is gone for clients that opt in, and the proof format did
not change.

- **Optional `X-Mentis-Content-Sha256`.** A read proof already signs the SHA-256 of the
  body. A client that also sends that digest in `X-Mentis-Content-Sha256: <64 lowercase
  hex>` lets the daemon verify the proof *before* reading the body and then hash the body
  as it streams. The signed body goes through. Any other body fails at its end with `400`
  before any handler has acted, so nothing is imported or changed, and memory use no
  longer depends on the body's size.
- **Without the header**, a read proof behaves exactly as before: the daemon reads up to
  1 MiB to check the signature. Its `413` now says how to send more.
- **`MENTISDB_MAX_BODY_BYTES`** (default 64 MiB, never below 1 MiB; library
  `MentisDbServiceConfig::with_max_body_bytes`) is the overall cap for every REST and MCP
  body. It also governs bearer-token requests, which still buffer the body up to the cap
  because the token check reads `chain_key` from it. Over the cap: `413`.
- **Clients.** The `mentisdb` CLI (`memory import` and every other command with a read
  key) and pymentisdb 0.10.1 send the header with every proof, so large imports just work:

  ```bash
  mentisdb memory import MEMORY.md --chain vault \
    --read-key-file builder.key --read-agent builder --read-key-id builder-1
  ```

- **Compatibility.** Existing proofs verify as before, so mentischat and older clients
  need no change, and a daemon that predates the header ignores it. If you sign requests
  yourself, add the header and hash exactly the bytes you send. Library:
  `read_auth::{body_digest, parse_content_digest, verify_request_for_digest,
  CONTENT_DIGEST_HEADER}`, plus a new `content_digest` fuzz target.

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

- **Reverse proxies** in front of MentisDB must allow request bodies up to
  `MENTISDB_MAX_BODY_BYTES` (default 64 MiB; nginx's `client_max_body_size` defaults to
  1 MiB). Otherwise large imports fail at the proxy. To keep the old ceiling, set
  `MENTISDB_MAX_BODY_BYTES=1048576`.
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

## Benchmarks

LoCoMo and LongMemEval were not re-run for this release: retrieval and scoring did not
change since 0.11.0.56, so [its numbers](mentisdb-0.11.0.56-release-notes.md#benchmarks)
stand. With the harness's new configurable timeouts (see
[Benchmark harness](#benchmark-harness)), LongMemEval can now also be run to completion on
throttled or slow machines.
