---
name: engineering-pipeline
description: MentisDB release engineering pipeline — ensures every release is regression-free, well-documented, and optimally parallelized.
triggers:
  - release pipeline
  - engineering pipeline
  - release checklist
  - publish mentisdb
  - ship mentisdb
  - version bump
---

# MentisDB Engineering Pipeline

Every release follows this pipeline in order. No step may be skipped.

## Before Committing

- Doc audit — verify all public API changes are reflected in docs/, README.md, docs.mentisdb.com, and docs.rs. Check: MCP tool catalog, REST endpoints, new ThoughtType variants, new config env vars, pymentisdb API coverage.

## Phase 1 — Plan for Parallel Execution

Design the release as independent work items that can be dispatched to parallel sub-agents:

1. **Decompose** the release into workstreams with zero shared mutable state (e.g., "search scoring changes" vs "REST endpoint additions" vs "documentation updates").
2. **Write each workstream as a self-contained task** with clear inputs, expected outputs, and verification commands.
3. **Checkpoint aggressively** — each agent must write `Summary` with `role: Checkpoint` to MentisDB before context compaction or handoff. Keep context usage below 50% at all times.
4. **Hand off via MentisDB memory** — the next agent reads the checkpoint, not the previous agent's raw context. Use `mentisdb_recent_context` to resume, never copy-paste between instances.
5. **Granular commits per workstream** — each agent commits its own completed workstream with a descriptive message before finishing.

## Phase 2 — Build, Lint, Test, CI

After each logical group of code changes, commit and push. The code must compile cleanly before pushing. After all code changes are done, run the full local quality gate:

```bash
cargo fmt
cargo clippy --all-features -- -D warnings
cargo test --all-features
```

**All three must pass with zero warnings and zero failures.** If any fail, fix the code and re-run until clean.

Then verify the **GitHub CI jobs pass** on the pushed commits:

```bash
gh run list --limit 3
gh run watch
```

CI may catch issues that local runs miss (different platform, different Rust version, stricter checks). **Do not proceed to Phase 3 until CI is green.** If CI fails, fix the code, push, and re-verify.

## Phase 3 — Benchmarks & Regression Testing

### Before benchmarking — daemon binary swap

`cargo build --release` produces a new binary while the **running** daemon is still the old one. You must restart the daemon after building to benchmark the correct version:

```bash
# Build first
cargo build --release

# Stop old daemon, start new one
pkill mentisdb
MENTISDB_DIR=~/.cloudllm/mentisdb nohup target/release/mentisdb > /tmp/mentisdb.log 2>&1 &
sleep 2
curl -sf http://localhost:9472/health || exit 1  # verify it's up
```

**Always verify the running daemon is the correct binary** by checking the process start time or checking `/proc/$(pgrep mentisdb)/exe`.

### Benchmark execution — use subagents

Run all long-running benchmarks via the `Task` tool as subagents, never directly in the shell. Background Python processes (LoCoMo, LongMemEval) get killed when the shell session ends, even with `nohup`. Subagents keep polling until completion.

Launch the benchmark as a subagent and wait for it to return the result line (e.g., "LoCoMo R@10: ...").

### LoCoMo regression detection

LoCoMo has non-trivial run-to-run variance (±1–2pp). Treat a **single run below baseline** as a possible signal, not confirmed regression. To confirm a real regression:

1. Run at least two full benchmark passes on the same binary.
2. If both runs are ≥0.5pp below baseline, investigate and fix before proceeding.
3. If only one run is below baseline, run a third to confirm.

### Scoring iteration — smoke test first

When iterating on scoring parameters (BM25 cutoffs, vector boost, decay rate, etc.), a full 1977-query LoCoMo run takes ~15–20 min. Before committing to a full run:

1. Run a **smoke test** on the first 200 queries only:
   ```bash
   python3 locomo-benches/locomo_bench.py --top-k 10 --chain smoke-$(date +%s) --data-dir data --workers 8 --limit 200
   ```
2. If smoke test improves or holds, run the full benchmark as a subagent.
3. If smoke test regresses significantly, tune parameters before running full benchmark.

### Full benchmark sequence

1. Start the release daemon on a fresh chain.
2. **Run LoCoMo benchmark**:
   ```bash
   python3 locomo-benches/locomo_bench.py --top-k 10 --chain locomo-$(date +%s) --data-dir data --workers 8
   ```
3. Compare against baseline. If R@10 regresses more than 0.5pp (confirmed across multiple runs), **investigate and fix** before proceeding.
4. **Run LongMemEval benchmark**:
   ```bash
   bash lme-benches/run_longmemeval.sh
   ```
5. **Record results** — save benchmark outputs to `results/` and note R@10 / R@5 in the changelog. If LongMemEval R@5 regresses more than 2pp from baseline, note the regression in the release notes.
6. **If regression detected** — use the systematic debugging skill: test the baseline binary on the same chain first to confirm the regression is from code changes and not from chain state differences (vector sidecar freshness, data mutations). Never assume a regression is from code without verifying.

**Regression investigation rules:**
- Always test the baseline binary on the same chain/data first.
- Vector sidecar freshness and chain data mutations can cause apparent regressions unrelated to code.
- Per-field DF cutoffs for BM25 must use **global DF** (postings list length), not per-field DF, for the cutoff comparison. Per-field DF is always ≤ global DF, so more terms pass through, adding noise.
- Use `--chain KEY` directly with the Python benchmark script, not via `run_locomo.sh EXTRA_ARGS` (the shell script generates its own chain key that may override).
- LoCoMo ingestion takes >2 minutes — run benchmarks in background with `nohup`.
- RRF reranking (k=50) is neutral on LoCoMo — it may help on other datasets where lexical and vector signals disagree on top candidates.

### LongMemEval is always post-release

LongMemEval results (~45 min for 500 instances) are not available at blog-post time for the initial release. The GitHub release notes can be edited after shipping to add LME numbers. Update the release notes via:
```bash
gh release edit TAG --notes "$(cat <<'EOF'
<updated content with benchmark results>
EOF
)"
```

## Phase 4 — Code Review

Before any documentation updates, do a final review pass:

- **Security** — no secrets, credentials, or API keys in committed code. No unsafe without safety comments.
- **Safety** — no unwraps on user input, no panics in daemon paths, proper error propagation.
- **Performance** — no unnecessary allocations in hot paths, no O(n²) where O(n) suffices.
- **DRY** — eliminate redundancy. If two functions do nearly the same thing, extract the shared core. Elegance through compression is a core principle.
- **Tests** — new behavior must have tests. Tests live in `tests/`, not inline in `src/`. Integration tests in `tests/<feature>_tests.rs`, unit tests in the module they test with `#[cfg(test)]`.
- **Public API** — new public types and functions require rustdoc.
- **Serialization** — persistence changes must preserve explicit integrity checks.

## Phase 5 — Documentation & Release

Order matters. Complete each step before the next.

### 5a. README.md
Update if the release adds or changes:
- REST endpoints or MCP tools
- Public API surface
- Scoring behavior
- Benchmark numbers
- Version references

### 5b. MENTISDB_SKILL.md
Update if the release adds or changes:
- MCP tools (add/remove)
- Retrieval parameters or behavior
- New relation kinds
- New concepts agents need to know about

**Keep it succinct.** Every token in the skill file costs agents at spawn time. Aggressively compress. Remove anything agents can discover via `mentisdb_list_skills` or `mentisdb_recent_context`. Focus on what agents must know to be smart, avoid repeating mistakes, and save tokens.

### 5c. docs.mentisdb.com
Update the Leptos Rust component files (`src/components/user_docs.rs`, `agent_docs.rs`, `developer_docs.rs`) if the release adds or changes:
- REST endpoints
- MCP tools
- Feature behavior
- Version-specific improvements sections

### 5d. changelog.txt
Add a new section at the top with format:

```
MAJOR.MINOR.ITERATION.INCREMENT MONTH/DAY/YEAR
  - change type(scope): description
  - entries ordered most important to least important
```

Change types: `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `chore`.

### 5e. Version bump
Update `version` in `Cargo.toml` to `MAJOR.MINOR.ITERATION` (three components, matching crates.io convention). The fourth component (INCREMENT) is the git tag only.

### 5f. Blog post
Write in `docs/` (in the mentisdb repo, e.g. `docs/mentisdb-X.Y.Z.html`), following the style of existing posts. Include:
- Benchmark results table
- What changed and why
- Upgrade instructions (`cargo install mentisdb --force`)
- Links to GitHub release
Then add an entry to `docs/index.html` linking to the new post.

### 5g. ROADMAP.md
Move completed items to a "Shipped" section or remove them. Update benchmark numbers. Update competitive position table.

### 5h. Commit & push
Each documentation change gets its own granular commit:
```bash
git add <files> && git commit -m "docs: update <what> for X.Y.Z release"
git push
```

### 5i. Git tag
```bash
git tag MAJOR.MINOR.ITERATION.INCREMENT
git push origin MAJOR.MINOR.ITERATION.INCREMENT
```

### 5j. GitHub release
**CRITICAL: Edit the release notes BEFORE publishing to crates.io.** The `gh release create --generate-notes` auto-generates notes but does NOT include the blog post. You must edit immediately after creation.

Steps:
1. Create the release (do NOT use `--generate-notes` as primary — it will overwrite your blog content):
   ```bash
   gh release create TAG --title "MentisDB TAG"
   ```
2. Immediately edit with the blog post content in Markdown:
   ```bash
   gh release edit TAG --notes "$(cat <<'EOF'
   <full blog post content in markdown>
   EOF
   )"
   ```
3. Verify: `gh release view TAG --json isPrerelease` — must be `false`

**Do NOT mark the release as pre-release** — the update checker uses the GitHub releases API which only returns non-prerelease releases as "latest", so pre-release tags silently break `mentisdb update` for all users.

### 5k. crates.io publish
**Do this AFTER step 5j** — once the GitHub release has the correct notes and is verified non-prerelease:
```bash
cargo publish
```

### 5l. Doc audit
Verify all public API changes are reflected in docs/, README.md, docs.mentisdb.com, and docs.rs. Check:
- MCP tool catalog
- REST endpoints
- New ThoughtType variants
- New config env vars
- pymentisdb API coverage

## Version Numbering

Format: `MAJOR.MINOR.ITERATION.INCREMENT`

- **MAJOR.MINOR.ITERATION** — `Cargo.toml` version (three components, crates.io convention)
- **INCREMENT** — monotone release counter, git tag only (fourth component)
- Example: `0.8.6.32` → Cargo.toml has `0.8.6`, git tag is `0.8.6.32`

## Key Rules

1. **Never skip Phase 2** — clippy warnings are errors. All tests must pass.
2. **Never skip Phase 3** — a regression in benchmarks means stop and fix before shipping.
3. **Never skip Phase 5a–5l** — stale docs mean confused users and agents that don't use new features.
4. **Always compare benchmarks against the baseline on the same chain** — chain state matters. Fresh chains may differ from stored baselines.
5. **Always checkpoint to MentisDB before compaction** — agents must write `Summary` with `role: Checkpoint` so the next agent can resume without losing progress.
6. **Keep the skill file under 200 lines** — if it grows, compress or move details to the changelog/roadmap.
7. **DRY in code and docs** — if two places say the same thing, extract it once.
8. **Daemon binary and running process may differ** — always restart after `cargo build --release` before benchmarking.
9. **Run long benchmarks as subagents** — never directly in the shell; `nohup` alone is insufficient to keep Python benchmark processes alive across shell sessions.
10. **Confirm LoCoMo regressions across multiple runs** — variance is ±1–2pp; one run below baseline is a signal, not a verdict.
