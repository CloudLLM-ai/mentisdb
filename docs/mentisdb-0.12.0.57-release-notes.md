# MentisDB 0.12.0.57 (One copy of every vector, derived-state backlog, large signed requests)

**Date:** November 3, 2026

A focused follow-up to 0.11.0.56. It keeps every vector in memory exactly once and makes
exact search 4.3x faster, removes the two memory trade-offs 0.11.0.56 documented for large
chains, shows operators how far derived state is behind, and lifts the 1 MiB limit on
signed requests behind a body-admission layer that checks the caller before reading a
byte. The minor version changes because public library types changed (see
[Upgrading](#upgrading)); the proof format and the on-disk files did not.

## Highlights

- **One copy of every vector.** The sidecar, the exact index, the HNSW graph and the
  implicit-edge scanner now share one flat, aligned vector store instead of keeping their
  own copies, and the scanner's queue holds positions instead of vectors. At 100,000
  thoughts this removes about 150 MB per former copy.
- **Exact search 4.3x faster.** Exact top-10 search over 100,000 vectors takes 14.2 ms
  instead of about 65 ms, with recall unchanged on every benchmark run.
- **`derived_backlog` in the chain listing** shows embeddings in flight, parked or missing
  and the implicit-edge backlog for each chain.
- **Large imports, admitted safely.** A read proof can declare its body digest in
  `X-Mentis-Content-Sha256`, so a large `mentisdb memory import` with a read key works.
  Bodies are admitted before they are read. The token is checked first, ordinary routes
  take at most 2 MiB, and only the three import routes take up to `MENTISDB_MAX_BODY_BYTES`
  (64 MiB), and only from an authenticated caller. Imports and ordinary requests draw on
  separate budgets.
- **BREAKING for library users:** `VectorSidecarEntry::vector` is now a `VectorRow`,
  `VectorSidecar` and `MentisDbServiceConfig` can no longer be built with struct literals,
  and exact-search scores may differ from 0.11 by up to `1e-5`.

## One copy of every vector

0.11.0.56 moved the implicit-edge neighbour scan off the chain's write lock on chains above
4,096 entries, with two documented trade-offs: the scanner kept its own copy of every
vector (about 150 MB at 100,000 × 384 dimensions), and its queue had no cap. Both are gone,
and so is the exact index's separate copy.

- **A flat, aligned store** (84313b5, e7a1c46). Vectors live in 128-row, 64-byte-aligned
  chunks. The sidecar entries, the exact index, the HNSW graph (its items and exact-vector
  cache) and the implicit-edge scanner all hold handles (`VectorRow`) into the same chunks.
  Cloning an entry or a whole sidecar copies pointers, not floats, and a snapshot clones
  only `entries / 128` chunk pointers. An HNSW graph loaded from disk keeps the graph's own
  copy (the `hnsw` crate owns its items); only its cache is re-pointed. A rebuild packs rows
  as it embeds, so the corpus is never held twice during a rebuild.
- **A bounded edge queue** (5b6ccc7). The edge queue stores runs of positions and reads the
  vectors when a scan starts, so a backlog costs a few bytes however long it grows. It
  still never blocks an append and never drops an edge.

Heap growth over the append phase from removing the scanner's copy (counting allocator in the `embed_commit_latency`
bench, run at background priority):

| Chain size | Appends | 0.11.0.56 | 0.12.0.57 |
|---|---|---|---|
| 20,000 thoughts | 2,700 | 46.4 MiB | 11.9 MiB |
| 100,000 thoughts | 12,700 | 239.2 MiB | 56.3 MiB |

Removing the exact index's copy saves memory on top of that. Exact live heap of the whole
`embed_commit_latency` process (`compaction` scenario, counting allocator, 100,000 thoughts
× 384, exact backend, background priority), compared with the build before:

| Point | Before | After | Saved |
|---|---|---|---|
| after loading the fixture | 475.2 MiB | 332.4 MiB | 142.8 MiB (the exact index's copy) |
| after 12,700 appends | 531.5 MiB | 369.5 MiB | 162.0 MiB |

Process RSS is noisier than live heap. In the same bench after the fixture, the final 0.12
code used 519 MiB, against 561–716 MiB across two runs of the 0.12 code before the flat
vector store (7b68537, which already shares the scanner's vectors). The exact live-heap
figures above are the reliable measure.

The exact index also no longer grows under churn. A removed document's slot is reused, and
once replaced or removed rows outnumber the live ones (beyond 256), the live rows are
copied to a fresh store (7e7125f).

### Faster exact search

Exact search runs over the flat store with a lane-parallel dot product (84313b5). It is
portable safe Rust that the compiler turns into NEON on aarch64 and SSE2 on x86_64, with no
nightly features and no runtime CPU probing, and it gives the same bits on every platform.
Measured with `benches/exact_search.rs` (`VectorIndex::search`, top 10, one thread, 384
dimensions, p50 per query, background priority, baseline-new-baseline):

| Vectors | Before | After | Queries/s |
|---|---|---|---|
| 100,000 | 65.6 / 64.2 ms | 14.2 ms | 15.5 → 71.7 |
| 20,000 | 11.5 / 12.3 ms | 2.7 ms | 80 → 366 |

Scores equal the old sequential cosine within `1e-5` (float reassociation), so two
documents whose scores differed by less than that may swap places. The HNSW and
implicit-edge paths still use the sequential cosine, so they can score the same pair up to
`1e-5` apart from exact search (af19c9d). Recall on the full
benchmarks is identical (see [Benchmarks](#benchmarks)).

The scan's sustainable rate is about 155 appends per second at 100,000 thoughts and about
720 per second at 20,000 (4 threads, 384 dimensions, a shared machine). If appends outrun
it, nothing is blocked or lost: edges for the newest thoughts appear late, and the backlog
drains when appends slow down (12,475 queued at 100,000 thoughts drained in 80 s). The new
`derived_backlog` shows when that happens.

Sidecar files, their integrity digest and persisted HNSW graphs are byte-identical and load
unchanged: a `VectorRow` serializes exactly as the `Vec<f32>` did.

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
- **Three separate budgets,** so one class of request can never starve another:

  | Class | Pool (all peers) | One peer may hold | Charged |
  |---|---|---|---|
  | Large: an import route, from an allowed caller | `MENTISDB_MAX_INFLIGHT_BODY_BYTES` (default 4 × the cap) | 2 × the cap, at most 4 bodies | the declared length, reserved on arrival |
  | Small: every other request from a caller that is not anonymous | 64 MiB | 32 MiB | bytes as they arrive |
  | Anonymous: bearer tokens on, no valid token | 16 MiB | 8 MiB | bytes as they arrive |

  A reservation is returned as soon as the body has been read. A declared length costs
  nothing for small and anonymous requests. On a loopback bind every local agent is the
  same peer, so the 32 MiB small share is generous there; with bearer tokens off it stops
  one peer from taking the whole pool.
- **Time limits.** A body that delivers nothing for `MENTISDB_BODY_IDLE_TIMEOUT_SECS`
  (default 30, range 1 to 3600) fails. Small and anonymous bodies also have a total
  deadline of twice that (60 s by default). Every body, large imports included, must reach
  `MENTISDB_MIN_BODY_BYTES_PER_SEC` (default **64 KiB/s**) once its grace period (twice the
  idle timeout, 60 s by default) is over. The rate is judged per window, each one idle
  timeout long, and each window counts only its own bytes. Without that floor, a client
  could declare 64 MiB and send one byte every 29 s, holding an import slot indefinitely.
  Because each window is judged on its own, sending most of a body at once does not buy
  time to trickle the rest. Lower the floor for imports over slow links, or set `0` to turn
  it off (the idle timeout and deadlines still apply).
- **`408` and `503` everywhere.** A body that is too slow or stalls gets **`408`**. A body
  that does not fit its budget gets **`503` with `Retry-After`**. This holds on every
  route, with bearer tokens on or off. Before, handlers' own JSON parsing turned both into
  a generic `400`.
- **Bad settings stop the daemon.** A non-numeric or out-of-range
  `MENTISDB_MAX_BODY_BYTES`, `MENTISDB_MAX_INFLIGHT_BODY_BYTES`,
  `MENTISDB_BODY_IDLE_TIMEOUT_SECS` or `MENTISDB_MIN_BODY_BYTES_PER_SEC` stops the daemon at
  startup instead of silently falling back to a default. An in-flight budget below one maximum body is raised to one
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
- **New statuses.** A stalled or too-slow body (under `MENTISDB_MIN_BODY_BYTES_PER_SEC`,
  64 KiB/s by default, in any window after the first 60 s) gets `408`, and an exhausted
  body budget gets `503` with `Retry-After`, on every route. Retry the `503`. If you import
  over a link slower than about 0.5 Mbit/s, lower `MENTISDB_MIN_BODY_BYTES_PER_SEC` or set it
  to `0`.
- **Reverse proxies** in front of MentisDB must allow request bodies up to
  `MENTISDB_MAX_BODY_BYTES` on the import routes (nginx's `client_max_body_size` defaults to
  1 MiB). Otherwise large imports fail at the proxy.
- **Body settings are validated.** `MENTISDB_MAX_BODY_BYTES` must be 1048576 to
  1073741824, `MENTISDB_BODY_IDLE_TIMEOUT_SECS` 1 to 3600, and
  `MENTISDB_MIN_BODY_BYTES_PER_SEC` 0 to 1073741824 (`0` = off). Any other value stops the
  daemon at startup.
- **`derived_backlog`** is a new field in each `GET /v1/chains` chain summary.
- **`mentisdb --help`** no longer lists "Supported agents": any agent works over MCP (HTTP
  or stdio), REST or the CLI. The agents `mentisdb setup` can configure are listed in
  `mentisdb setup --help`.

### Library users: the vector store types

`cargo-semver-checks` does not see field and parameter type changes, so here is every
signature change:

- `mentisdb::search::VectorSidecarEntry::vector` is a `VectorRow`
  (`mentisdb::search::flat_store::VectorRow`), a handle into the flat store that
  dereferences to `&[f32]`, instead of a `Vec<f32>`. Reading it as a slice works as before.
  Call `.to_vec()` where you need a `Vec<f32>`, and `VectorRow::standalone(&values)` to make
  one.
- `VectorSidecarEntry::new` takes `impl Into<VectorRow>` for the vector. `Vec<f32>` and
  `&[f32]` still convert, but an argument whose type is only inferred (for example a bare
  `.collect()`) no longer compiles, so annotate it.
- `VectorSidecar` has a new private field, so it can no longer be built with a struct
  literal. Use `VectorSidecar::build`.
- New public API: `search::flat_store` (`VectorStore`, `VectorRow`, `dot`, `DOT_EPSILON`),
  `VectorIndex::{from_rows, upsert_row}`, `VectorSearchBackend::upsert_row` (provided, so
  implementors keep compiling) and `HnswBackend::{from_rows, share_cached_rows}`.

Before (0.11):

```rust
use mentisdb::search::VectorSidecarEntry;

let entry = VectorSidecarEntry::new(id, index, hash, vec![0.1, 0.2, 0.3]);
let owned: Vec<f32> = entry.vector.clone();
let entry2 = VectorSidecarEntry::new(id, index, hash, (0..3).map(|i| i as f32).collect());
let literal = VectorSidecarEntry { thought_id: id, thought_index: index,
    thought_hash: hash.to_string(), vector: vec![0.1, 0.2, 0.3] };
```

After (0.12):

```rust
use mentisdb::search::VectorSidecarEntry;
use mentisdb::search::flat_store::VectorRow;

let entry = VectorSidecarEntry::new(id, index, hash, vec![0.1, 0.2, 0.3]); // unchanged
let owned: Vec<f32> = entry.vector.to_vec();
let values: Vec<f32> = (0..3).map(|i| i as f32).collect(); // annotate inferred types
let entry2 = VectorSidecarEntry::new(id, index, hash, values);
let literal = VectorSidecarEntry { thought_id: id, thought_index: index,
    thought_hash: hash.to_string(), vector: VectorRow::standalone(&[0.1, 0.2, 0.3]) };
let slice: &[f32] = &entry.vector; // reading as a slice is unchanged
```

`entry.vector.clone()` still compiles, but it now copies a pointer and returns a
`VectorRow`, not a `Vec<f32>`.

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
    .with_body_idle_timeout(Duration::from_secs(60))
    .with_min_body_bytes_per_sec(16 * 1024);      // 0 turns the rate floor off
```

## Benchmarks

The ranking logic did not change since 0.11.0.56; only exact-search scores moved, by at
most `1e-5`. The release verification (1addbdf) checked recall with five full LoCoMo and
LongMemEval protocol runs on the 0.12 code: recall was identical to
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
