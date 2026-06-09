---
name: engineering-pipeline
description: MentisDB release engineering pipeline — ensures every release is regression-free, well-documented, and optimally parallelized. This skill loads the canonical ENGINEERING_PIPELINE.md from the project root.
triggers:
  - release pipeline
  - engineering pipeline
  - release checklist
  - publish mentisdb
  - ship mentisdb
  - version bump
---

# Engineering Pipeline Skill

This skill loads the canonical **`ENGINEERING_PIPELINE.md`** from the
project root. That file is the single source of truth for the
MentisDB release process.

## Quick reference (not a substitute for the full doc)

### Phase 0 — Before committing
- Doc audit: verify API changes in docs/, README, docs.mentisdb.com, docs.rs
- MCP tools, REST endpoints, ThoughtType variants, config env vars, pymentisdb API

### Phase 1 — Plan for parallel execution
- Decompose into workstreams with zero shared mutable state
- Self-contained tasks with inputs/outputs/verification commands
- Checkpoint aggressively (`Summary` + `role: Checkpoint`)
- Hand off via MentisDB memory (`mentisdb_recent_context`)
- Granular commits per workstream

### Phase 2 — Build, lint, test, CI
```bash
cargo fmt
cargo clippy --all-features -- -D warnings
cargo test --all-features
```
All must pass with zero warnings. Then verify CI green.

### Phase 3 — Benchmarks & regression
- Build release, restart daemon, verify running binary
- Run benchmarks as subagents (`nohup` alone is insufficient)
- LoCoMo smoke test (200 queries) before full run
- Full LoCoMo + LongMemEval sequence
- Confirm regressions across multiple runs (±1–2pp variance)

### Phase 4 — Code review
- Security (no secrets, no unsafe without comments)
- Safety (no panics in daemon paths)
- Performance (no O(n²) in hot paths)
- DRY (extract shared core)
- Tests in `tests/`, rustdoc on public API
- Persistence changes preserve integrity checks

### Phase 5 — Docs & release
Order: README → MENTISDB_SKILL.md → docs.mentisdb.com → changelog → version bump → blog post → ROADMAP → git tag → GitHub release → crates.io publish → doc audit.

### Version numbering
`MAJOR.MINOR.ITERATION.INCREMENT` — Cargo.toml has 3 components, git tag has 4.

### Key rules (memorize)
1. Never skip Phase 2 (clippy = errors)
2. Never skip Phase 3 (benchmark regression = stop and fix)
3. Never skip Phase 5a–5l (stale docs = confused users)
4. Benchmark against baseline on SAME chain
5. Checkpoint before compaction (`Summary` + `role: Checkpoint`)
4. Keep skill file under 200 lines
5. DRY in code and docs
5. Restart daemon after `cargo build --release`
6. Run long benchmarks as subagents
7. Confirm LoCoMo regressions across multiple runs

## To run the pipeline
Read **`ENGINEERING_PIPELINE.md`** in the project root. It is the
canonical, detailed, version-controlled document. This skill file
is only a loader and quick reference.
