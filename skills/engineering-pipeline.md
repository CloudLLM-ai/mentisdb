---
name: engineering-pipeline
description: MentisDB release engineering pipeline — parallel, verified, documented, published (crates.io, GitHub release binaries, ghcr.io Docker image, docs sites) and post-release audited.
triggers:
  - release pipeline
  - engineering pipeline
  - release checklist
  - publish mentisdb
  - ship mentisdb
  - version bump
  - docker image release
---

# MentisDB Engineering Pipeline

Every release follows these phases in order. Skips need an explicit owner
exception, recorded in MentisDB and in the release notes. Evidence comes before
completion claims. For a same-day small fix use `hotfix-release.md` instead.

## Phase 0 — Re-anchor and scope

1. Work from `~/workspace/mentisdb`. Bootstrap MentisDB chain `mentisdb`, reuse
   your existing agent identity, read recent context and this skill.
2. `git status --short --branch`; never overwrite or move unrelated local files.
   The owner keeps untracked work in the repo (e.g. `promo/`): leave it alone.
3. Pick the version from crates.io, not from local tags:
   `curl -s -A "mentisdb-release (contact)" https://crates.io/api/v1/crates/mentisdb`
   → `max_version`. Cargo gets 3 components (`0.10.10`), the tag 4 (`0.10.10.55`).
4. Check `df -h` before fanning out build agents (budget 5–10 GiB per isolated
   `CARGO_TARGET_DIR`; delete them when done).

## Phase 1 — Plan parallel execution

- Split workstreams by disjoint write scope; record an ownership `Constraint`
  in MentisDB; each agent tags its thoughts and names new tests `tests/<ws>_*.rs`.
- Self-contained briefs: exact paths, expected outputs, verification commands.
- Worktree agents push their branches after each commit; the coordinator merges,
  runs the gate on master, and pushes. Granular commits, one finding per commit.

## Phase 2 — Build, lint, test, CI

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo check --no-default-features
cargo check --no-default-features --features server,llm-extraction   # compat set
cargo package --features local-embeddings      # from a clean worktree, see Phase 6
```

After pushing, watch every workflow (`gh run list`, `gh run watch <id> --exit-status`):
Tests, Clippy, Build (incl. the glibc-guarded `-compat` Linux jobs) and, on PRs
touching Docker, the `Docker image` PR build for amd64 + arm64.

## Phase 3 — Benchmarks and regression

Run unless the owner explicitly skips them (record the exception). Build release
in a scratch worktree, fresh daemon + fresh chain keys, LoCoMo and LongMemEval
default and rerank; compare against the stored baseline. If a metric regresses,
bisect with score-only runs against a copy of the baseline-ingested data.

## Phase 4 — Code review

Two independent reviews of `git diff <last-tag>..HEAD`, given the code, not your
conclusions: security/adversarial (auth scoping, SSRF, XSS/CSRF, secrets in logs,
Docker build context, workflow permissions and untrusted input) and
correctness/performance (integration seams between workstreams, hot paths,
shutdown/flush paths, test quality). Fix Critical/High before docs.

## Phase 5 — Documentation

Update only what changed, granular commits:
1. `README.md` (install incl. binaries table + Docker section, env vars, API).
2. `MENTISDB_SKILL.md` only for agent-critical changes (keep < 200 lines).
3. docs.mentisdb.com (`../docs.mentisdb.com`, Leptos): user/developer/agent docs,
   crate pin; `cargo check --features ssr` and hydrate wasm32 must pass.
4. `changelog.txt` (`X.Y.Z.N MON/DD/YYYY`), release notes
   `docs/mentisdb-X.Y.Z.N-release-notes.md`, blog `docs/mentisdb-X.Y.Z.N.html`,
   `docs/index.html`, `ROADMAP.md`, `WHITEPAPER.md/.tex` + `./build-whitepaper.sh`.
5. mentisdb.com (`../mentisdb.com`): fallback version in `src/github.rs`.
6. Security notices describe impact and remediation, never an exploit recipe.

Hold `changelog.txt` and the docs-site crate pin until the tag/publish step:
the changelog drives the live mentisdb.com version badge.

## Phase 6 — Release, publish, audit

1. Push master; confirm all CI green on the exact release commit.
2. Tag and release:
   ```bash
   git tag -a X.Y.Z.N -m "MentisDB X.Y.Z.N" <sha> && git push origin X.Y.Z.N
   gh release create X.Y.Z.N --title "..." --notes-file docs/mentisdb-X.Y.Z.N-release-notes.md --verify-tag
   gh release view X.Y.Z.N --json isDraft,isPrerelease   # both false
   ```
3. Publish to crates.io from a clean worktree (never `--allow-dirty`, never move
   the owner's files):
   ```bash
   git worktree add --detach <scratch>/publish X.Y.Z.N
   (cd <scratch>/publish && cargo publish --features local-embeddings)
   git worktree remove --force <scratch>/publish
   ```
4. The tag triggers two workflows; watch both to green:
   - `Build`: attaches 7 release assets.
   - `Docker image` (`.github/workflows/docker-publish.yml`): builds amd64 and
     arm64 natively, then publishes `ghcr.io/cloudllm-ai/mentisdb` as `X.Y.Z.N`,
     `X.Y.Z` and `latest`. It refuses tags whose first three components differ
     from `Cargo.toml`, so bump Cargo before tagging.
5. Verify binary assets (distinct, nonzero):
   `gh release view X.Y.Z.N --json assets --jq '.assets[] | "\(.name) \(.size)"'` →
   `mentisdb-linux-{x86_64,arm64}` (glibc ≥ 2.39, full),
   `mentisdb-linux-{x86_64,arm64}-compat` (glibc ≤ 2.35 guard, no local
   embeddings or startup sound), `mentisdb-macos-arm64`, `mentisdb-macos-x86_64`
   (no local embeddings), `mentisdb-windows-x86_64.exe`.
6. Verify the Docker image (no Docker daemon needed):
   ```bash
   gh api /orgs/CloudLLM-ai/packages/container/mentisdb --jq .visibility   # must be "public"
   gh api /orgs/CloudLLM-ai/packages/container/mentisdb/versions \
     --jq '.[0].metadata.container.tags'          # X.Y.Z.N, X.Y.Z, latest
   tok=$(curl -s "https://ghcr.io/token?scope=repository:cloudllm-ai/mentisdb:pull" | jq -r .token)
   curl -s -H "Authorization: Bearer $tok" \
     -H "Accept: application/vnd.oci.image.index.v1+json" \
     https://ghcr.io/v2/cloudllm-ai/mentisdb/manifests/X.Y.Z.N | jq '[.manifests[].platform]'
   ```
   The anonymous pull must return linux/amd64 and linux/arm64. A 403 means the
   package is private: the owner must set it public in the package settings
   (GitHub has no API for package visibility). With a working Docker daemon also
   smoke-test: `docker run --rm ghcr.io/cloudllm-ai/mentisdb:X.Y.Z.N --help`.
7. `cargo search mentisdb --limit 1` shows `X.Y.Z`.
8. Push the held docs-site pin and mentisdb.com commits. Verify live pages by
   content that changed in this release (e.g. the docs `/developer` crate pin),
   not by the version badge, which reads `changelog.txt` at runtime. Railway
   deploys from GitHub; if nothing deploys, check Railway status and integration.
9. Reply to and close the fixed issues; credit reporters and contributors.
10. `git status --short --branch` clean in every touched repo, no stray
    worktrees or remote branches; append a MentisDB `TaskComplete` with commits,
    CI run ids, release URL, crates.io version, asset inventory, image tags and
    platforms, and any exceptions.

## Key rules

- No completion claims without fresh command evidence; no silent skips.
- Never proceed past failed gates, failed CI, missing assets, a missing or
  private image, or an ambiguous publish state.
- Never move, hide or delete the owner's untracked files to satisfy tooling.
- If a release check finds a pipeline gap, fix the artifact first, then update
  this skill (repo copy and the MentisDB skill registry) so it cannot repeat.

## Git / commit

Every commit message ends with the authoring model attribution, e.g.
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
