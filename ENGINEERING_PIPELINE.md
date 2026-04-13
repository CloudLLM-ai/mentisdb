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

1. **Start the release daemon** with the current binary on a fresh chain.
2. **Run LoCoMo benchmark** against the fresh chain:
   ```bash
   python3 locomo-benches/locomo_bench.py --top-k 10 --chain locomo-$(date +%s) --data-dir data --workers 8
   ```
3. **Compare against the baseline** stored in `results/`. If R@10 regresses more than 0.5pp from the baseline, **investigate and fix** before proceeding.
4. **Run LongMemEval benchmark**:
   ```bash
   bash lme-benches/run_longmemeval.sh
   ```
5. **Record results** — save benchmark outputs and note R@10 / R@5 in the changelog and blog post.
6. **If regression detected** — use the systematic debugging skill: test the baseline binary on the same chain first to confirm the regression is from code changes and not from chain state differences (vector sidecar freshness, data mutations). Never assume a regression is from code without verifying.

**Regression investigation rules:**
- Always test the baseline binary on the same chain/data first.
- Vector sidecar freshness and chain data mutations can cause apparent regressions unrelated to code.
- Per-field DF cutoffs for BM25 must use **global DF** (postings list length), not per-field DF, for the cutoff comparison. Per-field DF is always ≤ global DF, so more terms pass through, adding noise.
- Use `--chain KEY` directly with the Python benchmark script, not via `run_locomo.sh EXTRA_ARGS` (the shell script generates its own chain key that may override).
- LoCoMo ingestion takes >2 minutes — run benchmarks in background with `nohup`.
- RRF reranking (k=50) is neutral on LoCoMo — it may help on other datasets where lexical and vector signals disagree on top candidates.

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

**Do NOT mark the release as pre-release** — the update checker uses the GitHub releases API which only returns non-prerelease releases as "latest", so pre-release tags silently break `mentisdbd update` for all users.

### 5k. crates.io publish
**Do this AFTER step 5j** — once the GitHub release has the correct notes and is verified non-prerelease:
```bash
cargo publish
```

## Version Numbering

Format: `MAJOR.MINOR.ITERATION.INCREMENT`

- **MAJOR.MINOR.ITERATION** — `Cargo.toml` version (three components, crates.io convention)
- **INCREMENT** — monotone release counter, git tag only (fourth component)
- Example: `0.8.6.32` → Cargo.toml has `0.8.6`, git tag is `0.8.6.32`

## Key Rules

1. **Never skip Phase 2** — clippy warnings are errors. All tests must pass.
2. **Never skip Phase 3** — a regression in benchmarks means stop and fix before shipping.
3. **Never skip Phase 5a–5e** — stale docs mean confused users and agents that don't use new features.
4. **Always compare benchmarks against the baseline on the same chain** — chain state matters. Fresh chains may differ from stored baselines.
5. **Always checkpoint to MentisDB before compaction** — agents must write `Summary` with `role: Checkpoint` so the next agent can resume without losing progress.
6. **Keep the skill file under 200 lines** — if it grows, compress or move details to the changelog/roadmap.
7. **DRY in code and docs** — if two places say the same thing, extract it once.