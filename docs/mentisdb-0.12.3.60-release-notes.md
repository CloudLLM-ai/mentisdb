# MentisDB 0.12.3.60 (Hotfix: settings that survive a restart, filters that filter)

**Date:** November 13, 2026

A hotfix on top of [0.12.2.59](mentisdb-0.12.2.59-release-notes.md), from the independent
reviews of that release. **Upgrade recommended**, especially if you change settings from the
dashboard's Settings screen.

**Security notice:** before this release, saving a setting from the dashboard could leave a
`.env` file that the daemon only half-loads at its next start. Depending on what was saved and
where the security settings sat in the file, the daemon could restart **without its dashboard
PIN or with bearer-token enforcement off**, and said nothing about it. See
[Security](#security) for what to check.

There is no data migration and no library break.

## Security

**Impact.** The Settings screen wrote values into `<MENTISDB_DIR>/.env` as raw text, and the
daemon ignored errors when loading that file. The `.env` parser stops at the first line it
cannot read and drops everything after it. A value with a space (the webhook CIDR list's own
example, `100.64.0.0/10, 10.8.0.0/24`, or a path like `/Users/x/My Data`), a `"`, or a ` #`
was enough. Every setting after that line, `MENTISDB_DASHBOARD_PIN` and
`MENTISDB_BEARER_TOKEN_ACCESS` among them, was silently unset on restart. A PIN containing `$`
was also loaded as a different PIN (`pa$word#1` became `pa#1`), and a PIN of only spaces saved
fine but loaded as no PIN at all.

**Fixes.**

- Values are written so they load back exactly: plain when safe, otherwise as an escaped
  double-quoted string. 300,000 adversarial values were fuzzed against the real parser with
  no mismatch, and no value can change another line.
- Every save is parsed back before it replaces the file: each changed setting must load as
  the value you entered and every other setting must keep its value, or nothing is written.
  A file that is already broken is refused (naming the line, never its text) instead of
  edited around.
- The file is replaced atomically (synced temp file, mode 0600, rename), never truncated in
  place. A save aborts if the existing file cannot be read.
- `export NAME=` and `NAME =` lines are updated in place. A rotated PIN used to be appended
  after an `export` line and silently ignored.
- The daemon **refuses to start** (exit code 2, naming the file and line) when a `.env` it
  loads has a bad line with a `MENTISDB_` setting on or after it, instead of running with
  those settings unset. A bad line in another tool's `.env` in a parent directory, with
  nothing of ours behind it, only warns.
- A blank or whitespace-padded PIN is refused. The PIN is no longer printed in the terminal
  config pane, at startup or after a save.

**What to check.** If you saved settings from the dashboard on 0.12.2.59 or earlier, open
`<MENTISDB_DIR>/.env` (default `~/.cloudllm/mentisdb/.env`) and confirm `MENTISDB_DASHBOARD_PIN`
and `MENTISDB_BEARER_TOKEN_ACCESS` are what you expect. After upgrading, if the daemon refuses
to start with `refusing to start with settings missing`, fix the named line by hand: that
file was already dropping your settings.

## Fixes

- **`source_episode` and `entity_type` filters now work with other filters.** Combined with
  any indexed filter (tags, concepts, types, roles, agents, owners), they were silently
  dropped, so `--tag foo --source-episode conv-9` returned every `foo` thought from every
  episode. This affected plain, ranked and federated search, context bundles, traversal and
  the CLI.
- **`source_episode` is trimmed** at append and in the filter, as `entity_type` already was,
  so `"X "` finds `"X"`. Values stored with padding before this release keep it.
- **Settings are checked before they are saved.** Every value is validated by the same rules
  the daemon applies at startup: whole numbers, 0–1 thresholds, ports, body limits, CIDR
  lists, an IP address for `MENTISDB_BIND_HOST`, and the choice settings. A bad value used to
  be saved and either stop the next start (with the dashboard gone, so it could not be
  repaired) or be silently replaced by the default. The whole request is checked before
  anything is applied. Clearing a field now unsets the setting instead of writing `NAME=`.
- **Oversized thoughts are refused before the expensive work.** A thought whose content
  alone exceeds the 10 MiB record cap is refused before signature checks, hashing and
  serialization, which ran under the chain's write lock.
- `MENTISDB_HNSW_EF_SEARCH` is marked as needing a restart (it is read once per process); a
  NUL byte in a setting is refused instead of crashing the handler; a filesystem that
  refuses a directory sync no longer fails an already-completed save.

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.12.3.60`.
There is no data migration.

- **Restart the daemon.** If it exits with code 2, see [What to check](#security).
- **Library:** `mentisdb::paths::load_dotenv_files` keeps its signature and now warns on
  stderr about a bad file; `try_load_dotenv_files` returns the error the daemon acts on.
- **Settings screen:** values the daemon would misread are now refused with a message per
  setting. `MENTISDB_BIND_HOST` must be an IP address.

### Remaining limitations

- A blank `source_episode` or `entity_type` filter matches nothing rather than meaning "no
  filter".
- `source_episode` has no length limit yet, and is not covered by the thought hash (it is
  covered by the signature).
- The MCP event stream checks its bearer token when it opens; a stream opened with a token
  that is later revoked stays open until it disconnects.

## Benchmarks

Retrieval is unchanged. On the same ingested chains, 0.12.2.59 and this release return
identical top-k lists with bit-identical scores for all 500 LongMemEval questions, default and
rerank, and identical LoCoMo results; append latency is unchanged. Details:
[`results/benchmarks/2026-11-13-0.12.3.60`](https://github.com/CloudLLM-ai/mentisdb/tree/master/results/benchmarks/2026-11-13-0.12.3.60).

| Benchmark | Metric | 0.12.2.59 | 0.12.3.60 |
|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 | 76.7 |
| LoCoMo-10P | rerank R@10 | 76.9 | 76.9 |
| LoCoMo-10P | default R@5 | 67.9 | 67.9 |
| LongMemEval | default R@5 | 69.8 | 69.8 |
| LongMemEval | rerank R@5 | 70.2 | 70.2 |

Full changelog: [`changelog.txt`](https://github.com/CloudLLM-ai/mentisdb/blob/0.12.3.60/changelog.txt)
