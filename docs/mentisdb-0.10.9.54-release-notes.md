# MentisDB 0.10.9.54 (Security, reliability and retrieval release)

**Date:** October 21, 2026

## Security notice — upgrade recommended

**Legacy MCP `/tools/execute` authorization bypass (affects 0.10.8.53 and earlier).**
When bearer-token access is enabled (`MENTISDB_BEARER_TOKEN_ACCESS=true`), a holder of a
*chain-scoped* token could use the legacy `/tools/execute` endpoint to:

- invoke tools against chains outside the token's scope (read and write),
- call global-only tools, such as listing every chain together with its storage path,
- merge or delete chains it was never granted.

Daemons that do not enable bearer-token access are not affected by this issue (without
tokens every client already has full access by design). If you issue chain-scoped tokens
to agents or users you do not fully trust, **upgrade now** and review recent activity on
your chains. Authorization now inspects exactly the request fields that are executed.

Other security fixes in this release:

- **Token scoping table.** REST and MCP now share one operation authorization table.
  Listing chains, webhook register/list/delete, cross-chain agent listing, skill
  mutations and `POST /v1/admin/flush` require a global token.
- **Merge/branch key normalization.** Chain keys in merge and branch requests are trimmed
  and validated before both authorization and execution, so a whitespace-padded key can
  no longer reach a different chain than the one authorized.
- **Webhook SSRF hardening.** Loopback, private, link-local, IPv4-mapped/translated, local
  NAT64 and Teredo targets are refused, each resolved address is re-checked at delivery
  time, redirects are not followed, and a circuit breaker pauses failing webhooks.
- **Dashboard.** Stored XSS through crafted chain/agent keys, CSRF on state-changing API
  calls and login, DNS rebinding (new `Host` allowlist), per-IP login rate limiting,
  PIN no longer returned by `GET /settings`, registry `storage_location` confined to the
  data directory, generic error bodies.
- **Backup restore bounds.** Manifest capped at 8 MiB, at most 100k files, declared sizes
  cross-checked against the zip central directory, free-space check on Unix, no restores
  through symlinks.
- **File permissions.** The dashboard `.env` (holds the PIN), integration-config backups
  and `.torn` files are written `0600`; integration temp files use unpredictable names.

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Restart the daemon. Behaviour changes you may need to act on:

- **Reads of an unknown chain return `404`.** Reads no longer create chains. Call
  `mentisdb_bootstrap` (or `POST /v1/bootstrap`) before reading a new chain.
- **Chain-scoped tokens are confined.** Operations listed above need a global token;
  scope denials return `401`. Set `MENTISDB_BEARER_TOKEN` to a global token so
  `mentisdb backup`/`restore` can flush the daemon.
- **Dashboard `Host` allowlist.** Every dashboard route answers `403` unless `Host` is
  `localhost`, `127.0.0.1`, `[::1]`, `my.mentisdb.com`, `MENTISDB_BIND_HOST`, a local
  interface IP when bound to `0.0.0.0`/`::`, or a name in
  `MENTISDB_DASHBOARD_ALLOWED_HOSTS` (comma-separated, environment or `.env`). Reverse
  proxies must preserve the original `Host` or forward an allowed name.
- **Scripted dashboard calls** (`POST`/`PATCH`/`DELETE` under `/dashboard/api/`) that send
  neither `Origin` nor `Referer` must add `X-MentisDB-Dashboard: 1`.
- **Mixed MCP payloads** carrying both `tool`/`parameters` and `name`/`arguments` return
  `400`.
- **REST bodies over 1 MiB** return `413` while bearer-token access is enabled.
- **Dashboard chain keys** must match `[A-Za-z0-9_-]{1,128}`.
- **`MENTISDB_EMBEDDINGS`** (`off`, `local-text`, `fastembed`) selects the embeddings mode
  process-wide; unset keeps the previous default. The ONNX model cache moves to
  `<MENTISDB_DIR>/.fastembed_cache` (override with `FASTEMBED_CACHE_DIR`); a stray
  `.fastembed_cache/` in your old working directory can be deleted.
- **`MENTISDB_FSYNC=1`** opts into an `fsync` per durable group commit (power-loss
  durability). Off by default: commits survive a process crash, and explicit flush,
  shutdown and drop always sync.
- **First start after upgrading** may serve lexical/graph-only ranked results on large
  chains while their vector sidecars rebuild in the background.
- **Intel macOS binary** is published again but built without `local-embeddings`
  (no FastEmbed); it uses `local-text` embeddings.
- The daemon now also loads `<MENTISDB_DIR>/.env` (after `./.env`), so settings saved from
  the dashboard apply regardless of the working directory.

## Fixes

### Memory blow-up / OOM after upgrading with many chains ([#23](https://github.com/CloudLLM-ai/mentisdb/issues/23))

Reported by [takver](https://github.com/takver), with a clear reproduction. Each chain
loaded its own FastEmbed ONNX session (~100 MB each, never released), and opening a chain
without a fresh sidecar embedded its entire history in one unbounded batch on the request
path. On a 13-chain, 2,704-thought reproduction the daemon exceeded a 6 GB cap (the
reporter hit OOM at 28.8 GiB). It now peaks at about **1.7 GB**.

- One FastEmbed model per process, shared by every chain; inference in small bounded
  chunks.
- Missing or stale sidecars are rebuilt by a single background worker (single-flight per
  sidecar: at most one job queued or running) and adopted when finished; the chain
  serves lexical and graph results meanwhile.
- Opening a chain never rewrites its persisted vector provider config.
- Model cache under `<MENTISDB_DIR>`, plus the `MENTISDB_EMBEDDINGS` opt-out.

### Retrieval

- **Rerank fixed.** `enable_reranking=true` replaced the additive score with the tiny raw
  RRF total and collapsed rankings (LoCoMo R@5 3.6%). RRF is now a scaled consensus bonus
  over positive-only lists.
- **Stemmed, damped thesaurus.** Synonyms are keyed by stem so inflected words expand, at
  weight 0.3 and at most 3 synonyms per term.
- **Federated search** keys RRF by (chain, thought id) and deduplicates before paging.
- **Filtered HNSW** searches use an exact scan or recall-aware oversampling.

### Durability

- A torn final record is preserved to a `.torn` file and truncated before the writer
  reopens the chain. Only a strict partial last record counts as torn; mid-file
  corruption fails open with `InvalidData` and nothing is dropped.
- Agent, entity-type and chain registries are written atomically; chain registration is
  serialized; migration rewrites go through a synced temp file.
- The implicit-edge overlay records the thought count and head hash it covers and
  catches up after a crash instead of losing up to 31 thoughts' edges.
- Opt-in `MENTISDB_FSYNC`; a lone durable append no longer waits for the group-commit
  window.

### Server and dashboard

- Chains open single-flight off the async workers; disk I/O runs on the blocking pool.
- Errors map to accurate statuses (`400/401/403/404/409/413/500`) without leaking
  internals; branching into an existing chain returns `409`.
- Skill upload no longer creates an unknown chain.
- Dashboard: stale fetches ignored, branch/merge dialogs and pickers refresh, skill edit
  and import counts fixed, paging clamped, 390px layout fixes.

### Build, CI and dependencies

- Intel macOS release binary returns (without `local-embeddings`); Actions on Node 24.
- `--no-default-features` builds compile again; LoCoMo harness passes `--reranking`
  through.
- Upgraded: ureq 3, sha2 0.11, ed25519-dalek 3, zip 9, crossterm 0.29, diffy 0.5,
  roaring 0.11, toml_edit 0.25, rodio 0.23, criterion 0.8.
- **fastembed intentionally stays on 5.x.** fastembed 7 brings ort 2.0.0-rc.15, which
  always registers the CoreML execution provider on macOS; it cut query throughput about
  2.5x. A guard test keeps the pin.

## Performance

- Per-append cost 6.3–7.8 ms → 1.4 ms (release, `MENTISDB_FSYNC` off).
- Ranked query paths: score-only top-k, dense lexical scans, cached vector norms, arena
  graph expansion. Criterion: ranked lexical 997 µs → 96 µs, filtered lexical
  138 µs → 11 µs, heuristic 1.52 ms → 94 µs.

## Benchmarks

Fresh isolated daemons, release builds, `fastembed-minilm` sidecar. BEFORE is the 0.10.8
code line plus dependency refresh (`b4372ff`); FINAL is the release code (`f9eca14`).

| Metric | BEFORE | FINAL |
|---|---|---|
| LoCoMo default R@5 / R@10 | 63.8 / 72.7 | 63.9 / 73.5 |
| LoCoMo rerank R@5 / R@10 | 3.6 / 5.9 | 66.0 / 74.1 |
| LongMemEval default R@5 / R@10 / R@20 | 66.8 / 72.0 / 78.2 | 68.0 / 72.8 / 78.8 |
| LongMemEval rerank R@5 | 2.8 | 68.6 |
| Query throughput LoCoMo / LongMemEval | ~206 / ~159 q/s | ~384 / ~385 q/s |
| Ingest LoCoMo | 171.5 s | 75.1 s |
| Ingest LongMemEval | 725.6 s | 184.2 s |

Ingest times are noisy: the host was under varying load during these runs. A bisect of
an interim build found two default-path regressions (thesaurus weight after the stem-key
fix, and fastembed 7 enabling CoreML); both were fixed before release.

Fixes: [#23](https://github.com/CloudLLM-ai/mentisdb/issues/23).
