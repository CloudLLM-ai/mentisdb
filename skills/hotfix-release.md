---
name: hotfix-release
description: Fast-path release pipeline for hotfixes — bump version, update all version surfaces, rebuild PDF, commit, tag, release, publish. Skips benchmarks and full review when the change is a small fix on top of a just-shipped release.
triggers:
  - hotfix release
  - hotfix re-release
  - re-release
  - quick release
  - patch release
---

# MentisDB Hotfix Release

Use this when a small fix needs to ship immediately on top of a just-released
version. It skips Phase 3 (benchmarks) and Phase 4 (code review) since the
code change is minimal and was already reviewed.

## When to use

- A bug was found in a just-shipped release (same day or within hours).
- The fix is small (a few lines), no scoring changes, no new features.
- Benchmarks from the prior release are still valid (no retrieval-affecting
  code changed).

## When NOT to use

- The fix changes scoring, retrieval, or vector behavior — run the full
  `engineering-pipeline` skill instead.
- The fix is large or touches many files — run the full pipeline.
- More than a day has passed and other changes have accumulated — run the
  full pipeline.

## Prerequisites

- The prior release was shipped via the full engineering pipeline (Phases
  1–5 complete).
- The hotfix code is already committed and pushed to `master`.
- CI is green on the hotfix commit.

## Steps

### 1. Determine the new version

Two options depending on severity:

**Option A — bump ITERATION (new Cargo.toml version):**

```
OLD = 0.10.2.47
NEW = 0.10.3.48
```

Cargo.toml: `0.10.2` → `0.10.3`. Use this when the fix is user-visible
and warrants a crates.io version bump (auto-updater will pick it up).

**Option B — bump INCREMENT only (same Cargo.toml version):**

```
OLD = 0.10.2.47
NEW = 0.10.2.48
```

Cargo.toml stays `0.10.2`. Use this for trivial fixes where a crates.io
republish isn't needed. The git tag still advances.

### 2. Update all version surfaces

Find and replace the old version string with the new one:

| File | What to update |
|------|---------------|
| `Cargo.toml` | `version = "X.Y.Z"` (three components, Option A only) |
| `changelog.txt` | Top section header: `X.Y.Z.N DATE` |
| `WHITEPAPER.tex` | `\small Version X.Y.Z.N` line |
| `WHITEPAPER.md` | `**Version:** X.Y.Z.N` |
| `ROADMAP.md` | Shipped section header + any inline references |
| `docs/mentisdb-X.Y.Z.N.html` | `git mv` old → new filename; update title, h2, benchmark table, GitHub release link |
| `docs/index.html` | Update the `<a href>` link and display text |

Find stragglers:

```bash
grep -rn 'OLD_VERSION' --include='*.md' --include='*.tex' --include='*.html' --include='*.txt' .
```

### 3. Rebuild WHITEPAPER.pdf

```bash
./build-whitepaper.sh
```

Verify the build succeeds and the byte count is reasonable.

### 4. Add changelog entry for the hotfix

Add the fix description to the top of the changelog section:

```
  - fix(scope): what was wrong and what changed
```

### 5. Quality gate

```bash
cargo fmt --check
cargo clippy --all-features -- -D warnings
cargo test --all-features
```

All three must pass. This verifies the version bump didn't break
anything (e.g. Cargo.lock update).

### 6. Commit & push

```bash
git add <all updated files>
git commit -m "release: X.Y.Z.N hotfix — <one-line summary>"
git push origin master
```

### 7. Git tag

```bash
git tag X.Y.Z.N
git push origin X.Y.Z.N
```

### 8. GitHub release

```bash
gh release create X.Y.Z.N --title "MentisDB X.Y.Z.N"
```

Immediately edit with release notes. For a hotfix:

- Lead with what was fixed and why.
- Include a **Hotfix** section explaining the fix.
- Reference the prior release for full feature list and benchmark numbers.
- Include upgrade instructions.

```bash
gh release edit X.Y.Z.N --notes "$(cat <<'EOF'
<markdown content>
EOF
)"
```

Verify it's not pre-release:

```bash
gh release view X.Y.Z.N --json isPrerelease
```

### 9. Publish to crates.io

```bash
cargo publish --allow-dirty
```

Use `--allow-dirty` only if there are untracked files not part of the
release (e.g. `.grok/`, local config). If the working directory is
clean, omit the flag.

Verify:

```bash
cargo search mentisdb
```

## Key Rules

1. **Skip benchmarks only if no retrieval/scoring/vector code changed.**
   If it did, run the full pipeline.
2. **Update ALL version surfaces** — stale version numbers create
   confusion. Use `grep -rn` to find stragglers.
3. **Rebuild the PDF** — the `.tex` version change is meaningless without
   a fresh `.pdf`.
4. **The blog post gets renamed, not duplicated** — `git mv` the old
   file, then update its contents.
5. **GitHub release notes must mention it's a hotfix** — users need to
   know whether to upgrade.
6. **CI should already be green** from the hotfix code commit. The
   version bump commit is docs-only.

## Version history

- v1 (2026-06-20): extracted from the 0.10.3.48 hotfix release.
