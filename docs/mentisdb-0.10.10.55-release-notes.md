# MentisDB 0.10.10.55 (Private webhook targets, official Docker image, Linux compat binaries)

**Date:** October 22, 2026

## Highlights

- **Private webhook targets (`MENTISDB_WEBHOOK_ALLOWED_CIDRS`).** Self-hosted deployments
  whose webhook consumers live on a private network (WireGuard, Tailscale, a LAN subnet)
  can now opt specific ranges back in past the SSRF guard that 0.10.9 introduced
  ([#22](https://github.com/CloudLLM-ai/mentisdb/issues/22)).
- **Official Docker image.** Multi-arch `ghcr.io/cloudllm-ai/mentisdb` for `linux/amd64`
  and `linux/arm64`, published on every release. Contributed by
  [@nycterent](https://github.com/nycterent) in
  [#16](https://github.com/CloudLLM-ai/mentisdb/pull/16), finished in
  [#24](https://github.com/CloudLLM-ai/mentisdb/pull/24).
- **Linux binaries for older distros.** New `mentisdb-linux-x86_64-compat` and
  `mentisdb-linux-arm64-compat` assets run on glibc 2.34+ (Ubuntu 22.04 LTS, Debian 12,
  RHEL 9), without local embeddings
  ([#21](https://github.com/CloudLLM-ai/mentisdb/issues/21)).
- **Graceful SIGTERM.** `mentisdb --headless` drains its servers and exits `0` on SIGTERM,
  so `docker stop`, Kubernetes and systemd shut it down cleanly. The interactive TUI also
  shuts down cleanly on SIGTERM (`kill`, systemd, `docker run -t` + `docker stop`).

## Private webhook targets

0.10.9 refuses webhook targets that resolve to loopback, private, link-local and other
internal ranges. That is the right default, but it left no way to deliver to a consumer
on a private overlay network. Set `MENTISDB_WEBHOOK_ALLOWED_CIDRS` to the ranges you
trust:

```bash
# Tailscale (IPv4 CGNAT range + IPv6 ULA)
MENTISDB_WEBHOOK_ALLOWED_CIDRS=100.64.0.0/10,fd7a:115c:a1e0::/48
# a WireGuard subnet
MENTISDB_WEBHOOK_ALLOWED_CIDRS=10.8.0.0/24
# one host
MENTISDB_WEBHOOK_ALLOWED_CIDRS=192.168.1.50
```

- Entries are CIDR ranges or single addresses, separated by commas and/or whitespace.
  Host bits are normalized (`10.8.0.7/24` means `10.8.0.0/24`).
- Only addresses inside a listed range are let through; every other internal address
  stays blocked. `localhost` host names are still rejected, and any entry broader than
  `/8` (IPv4 or IPv6; `fd00::/8` still fits) is refused, so the guard cannot be re-opened
  wholesale with something like `0.0.0.0/1`.
- IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) match IPv4 ranges.
- The allowlist applies everywhere the guard does: at registration, when persisted
  registrations are reloaded at startup, in the delivery-time DNS resolver, and in the
  literal-IP re-check.
- An invalid list fails daemon startup instead of being silently ignored, and the
  active ranges are logged as a warning at startup. A listed range that covers loopback
  (`127.0.0.1`, `::1`) or the cloud metadata address `169.254.169.254` gets its own
  warning.

Set it in the environment or in `./.env` / `<MENTISDB_DIR>/.env`. Only list ranges whose
hosts you are comfortable letting anyone who can register a webhook reach.

For library users: `WebhookUrlPolicy::AllowCidrs`, `WebhookUrlPolicy::from_env`,
`WebhookUrlPolicy::from_allowed_cidrs`, `WebhookUrlPolicy::permits_ip`, `WebhookCidr`,
`WEBHOOK_ALLOWED_CIDRS_ENV`, `MIN_ALLOWED_PREFIX_V4`, `MIN_ALLOWED_PREFIX_V6` and
`WebhookManager::policy` are new. `WebhookUrlPolicy` is
now `Clone` instead of `Copy`.

## Docker quickstart

```bash
docker run -d --name mentisdb \
  -p 127.0.0.1:9471:9471 -p 127.0.0.1:9472:9472 -p 127.0.0.1:9475:9475 \
  -v mentisdb-data:/var/lib/mentisdb \
  ghcr.io/cloudllm-ai/mentisdb:latest

curl http://127.0.0.1:9472/health
```

| Item | Value |
| --- | --- |
| Tags | `0.10.10.55` (release tag), `0.10.10` (crate version, moves with each release of that version), `latest` |
| Platforms | `linux/amd64`, `linux/arm64` |
| Base | `debian:trixie-slim` (glibc 2.41), so local embeddings work regardless of the host's glibc |
| Features | server + local embeddings + LLM extraction (no startup sound / audio) |
| User | non-root `mentisdb`, uid/gid `991`; bind mounts must be writable by uid 991 |
| Data | `/var/lib/mentisdb` volume (`MENTISDB_DIR`): chains, skills, TLS certificates, model cache |
| Ports | `9471` MCP, `9472` REST, `9475` dashboard (HTTPS); HTTPS MCP/REST off unless `MENTISDB_HTTPS_MCP_PORT` / `MENTISDB_HTTPS_REST_PORT` are set |
| Process | `mentisdb --headless` under `tini`; no TTY needed; `docker stop` drains cleanly |
| Health | Docker `HEALTHCHECK` on REST `/health` |
| Build context | `.dockerignore` keeps `.env*`, `*.pem`, `*.key` and keypair JSON out of local builds |
| Publishing | refused unless the release tag's first three components match `Cargo.toml`, so a stray tag cannot move `latest` |

The daemon binds `0.0.0.0` inside the container and starts without authentication.
Publish ports on `127.0.0.1` unless a TLS reverse proxy or VPN is in front, set
`MENTISDB_BEARER_TOKEN_ACCESS=true` for anything beyond the host (create tokens with
`docker exec mentisdb mentisdb bearertoken create --global admin`), and set
`MENTISDB_DASHBOARD_PIN`. A Compose example is in the
[README](https://github.com/CloudLLM-ai/mentisdb#docker).

## Which Linux binary?

| Asset | Local embeddings (FastEmbed) | Minimum glibc | Runs on |
| --- | --- | --- | --- |
| `mentisdb-linux-x86_64`, `mentisdb-linux-arm64` | yes | 2.39 | Ubuntu 24.04+, Debian 13+, Fedora 40+, RHEL 10, Arch |
| `mentisdb-linux-x86_64-compat`, `mentisdb-linux-arm64-compat` | no (and no startup sound) | 2.34 | Ubuntu 22.04 LTS, Debian 12, RHEL 9 and newer |
| Docker image | yes | any host | any Linux with Docker |

Check your glibc with `ldd --version`. A full binary on an older glibc fails at start
with ``version `GLIBC_2.39' not found``.

The full builds cannot go lower: the prebuilt ONNX Runtime that local embeddings link is
itself compiled against glibc 2.38+ and GCC 13 libstdc++. The `-compat` builds are
compiled in an `ubuntu:22.04` container without the `local-embeddings` and
`startup-sound` features, so they use lexical search and `local-text-v1` vectors (the same
trade-off as the Intel macOS binary) and need no ALSA (`libasound.so.2`) on minimal
servers. CI fails the release if a `-compat` binary ever needs a glibc newer than 2.35,
and smoke-tests it inside the old userland.

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.10.10.55`.
No data migration is needed and there are no breaking behaviour changes for existing
deployments.

- **Webhook targets** behave exactly as in 0.10.9 unless you set
  `MENTISDB_WEBHOOK_ALLOWED_CIDRS`. If you set it, a malformed entry stops the daemon at
  startup; fix the value rather than expecting it to be ignored.
- **Process supervisors**: the headless daemon now exits `0` on SIGTERM after draining,
  and the TUI also stops cleanly on SIGTERM.
  Supervisors that treated the old signal death as a failure no longer need to.
- **Moving from a third-party image or a hand-built container**: mount your existing data
  directory at `/var/lib/mentisdb` and make it writable by uid 991
  (`chown -R 991:991 <dir>`).
- **Linux users on Ubuntu 22.04 / Debian 12** who could not start the 0.10.9.54 binary
  should switch to the `-compat` asset, or to the Docker image to keep local embeddings.

## Benchmarks

LoCoMo and LongMemEval were not re-run for this release, by owner exception: nothing in
retrieval, scoring or embedding changed since 0.10.9.54, so its published numbers stand.
The `-compat` binaries and the Intel macOS binary use `local-text-v1` vectors and are not
covered by those numbers, which were measured with the `fastembed-minilm` sidecar.

## Credits

- [@nycterent](https://github.com/nycterent) for the Dockerfile and ghcr publishing
  workflow ([#16](https://github.com/CloudLLM-ai/mentisdb/pull/16)), which
  [#24](https://github.com/CloudLLM-ai/mentisdb/pull/24) carried to release.
- takver for reporting the glibc floor
  ([#21](https://github.com/CloudLLM-ai/mentisdb/issues/21)), requesting private webhook
  targets ([#22](https://github.com/CloudLLM-ai/mentisdb/issues/22)), and the 0.10.9.54
  memory report ([#23](https://github.com/CloudLLM-ai/mentisdb/issues/23)).

Fixes: [#21](https://github.com/CloudLLM-ai/mentisdb/issues/21),
[#22](https://github.com/CloudLLM-ai/mentisdb/issues/22).
