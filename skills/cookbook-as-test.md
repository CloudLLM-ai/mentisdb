---
name: cookbook-as-test
description: Pipeline for keeping MentisDB cookbook code examples in sync with the public API. Every Rust block in docs/cookbook/ is extracted, wrapped as a test, and compiled on every CI run. Use this skill whenever a chapter claims the library does X and you need to verify it.
triggers:
  - cookbook as test
  - extract cookbook examples
  - API drift in docs
  - docs test fail
  - cookbook_test fail
  - run cookbook test
---

# Cookbook-as-Test Pipeline

The MentisDB cookbook is documentation, but every `<pre><code>` Rust
block is also a **claim about what the library does**. If the API
drifts and a chapter references a method that no longer exists, the
cookbook becomes a lie. This pipeline catches that lie on every CI
run.

## When to use this skill

- You just changed a public API in `src/lib.rs` and want to know which
  cookbook chapters need updating.
- A sub-agent wrote a new chapter and you want to verify the code
  compiles before merging.
- You're debugging a `cargo test --test cookbook_tests` failure.
- You're adding a new chapter to the cookbook and want to know how
  to write code that will pass the pipeline.

## The pipeline in one diagram

```
   docs/cookbook/*.html  ──→  scripts/extract_cookbook_examples.py
        (HTML with             │
         <pre><code>           │ parse, filter, strip
         Rust blocks)          │ duplicates, decode
                               ▼
                       tests/cookbook/*.rs
                        (per-chapter test
                         modules)
                               │
                               ▼
                    cargo test --test cookbook_tests
                               │
                               ▼
                    Pass → CI green
                    Fail → drift surfaced, fix chapter
```

## Step 1: Mark illustrative blocks

Not every Rust block in the cookbook is meant to compile standalone.
Some are pseudocode, some define helpers that span multiple blocks,
some show output rather than code. Mark these so the extractor
skips them:

```html
<pre data-cookbook-test="off"><code>
// pseudocode, not compiled
</code></pre>
```

The extractor regex is:
`<pre(?![^>]*data-cookbook-test="off")[^>]*>...`

## Step 2: Write code that compiles

Code that **will** be compiled must:

1. Not re-`use` crates the test wrapper already pre-imports (the
   extractor strips these, but only for the standard set:
   `mentisdb`, `mentisdb::search`, `tempfile`, `uuid`, `std::io`,
   `std::collections`, `chrono`).
2. Avoid invented helper types. If you define `EpisodeRecorder` in
   block 1 and use it in block 2, that works because the extractor
   puts all blocks from a chapter in one module scope. But invented
   types in **one block only** (e.g. `HandoffEnvelope` defined and
   used in one snippet) work too. Invented types used in a separate
   test block will fail — mark the test block illustrative.
3. Use the actual public API. If unsure, check
   `skills/mentisdb-public-api-reference.md` or grep `src/lib.rs`.

## Step 3: Run the extractor

```bash
python3 scripts/extract_cookbook_examples.py docs/cookbook/
# Writes tests/cookbook_tests.rs and tests/cookbook/<chapter>.rs
```

The extractor does:

- Regex-match `<pre[^>]*><code[^>]*>(.*?)</code></pre>`
- Skip blocks with `data-cookbook-test="off"`
- Filter out non-Rust (shell, JSON, markdown, output)
- HTML-decode **after** stripping `<...>` tags (otherwise `Result<()>`
  gets eaten as a tag)
- Strip duplicate `use` lines (the wrapper pre-imports them)
- Emit one `.rs` per chapter with all blocks in one module scope

## Step 4: Run the test

```bash
cargo test --test cookbook_tests --all-features
```

A passing test is a guarantee that every code example in the
cookbook actually compiles against the current public API. A
failing test is a drift alert: a chapter claims the library
does something the library no longer does.

## Step 5: Diagnose failures

The failure modes, in order of frequency:

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `cannot find function X` | API renamed/removed | Check `mentisdb-public-api-reference.md`, update chapter |
| `the ? operator cannot be applied to RankedSearchResult` | `query_ranked` returns `RankedSearchResult` directly, not `Result` | Drop the `?` |
| `expected u64, found u32` | `with_refs` wants `u64`, not `u32` | Cast `as u64` |
| `with_since not found on RankedSearchQuery` | `with_since` is on `ThoughtQuery` | Move into `.with_filter(ThoughtQuery::new().with_since(...))` |
| `cannot find type X` | Chapter invented `X` (`EpisodeRecorder`, `HandoffEnvelope`, ...) | Either mark block illustrative, define `X` earlier in the same chapter, or remove the snippet |
| `RankedSearchScore doesn't impl Display` | `hit.score` is an ordered float | Use `{:?}` not `{}` |
| `? couldn't convert error to io::Error` | `manage_vector_sidecar` returns `VectorSearchError`, not `io::Error` | Use `.expect(...)` or match the error |

## Step 6: CI integration

`.github/workflows/test.yml` runs the extractor then the test:

```yaml
- name: Run cookbook-as-test
  run: |
    python3 scripts/extract_cookbook_examples.py docs/cookbook/
    cargo test --test cookbook_tests --all-features
```

Generated files (`tests/cookbook_tests.rs`, `tests/cookbook/*.rs`) are
gitignored — they're regenerated on every CI run.

## Writing new chapters that pass first try

A short checklist for sub-agents writing a new chapter:

1. Read `cookbook-1-1-episodic-task-memory.html` to match the HTML
   template exactly.
2. Read `mentisdb-public-api-reference.md` (this folder) for the
   current API names.
3. Write each code example as a self-contained snippet that would
   compile if pasted into a `fn main()`. If your example references
   a helper from earlier in the chapter, define that helper in the
   same chapter before the example.
4. After writing, run the extractor and the test locally. If it
   fails, fix the chapter — don't add `data-cookbook-test="off"`.
5. In your `TaskComplete`, include any API names you used that
   weren't in the public-API reference, so the next agent doesn't
   drift.

## Common pitfalls (from the dogfooding experiment)

- **Multi-block examples**: the chapter is one module scope, so a
  struct defined in block 1 can be used in block 2. But if you want
  one example to be testable on its own, give it a `fn main()` or
  a named function.
- **HTML-entity gotchas**: the extractor decodes `&lt;` after
  stripping `<...>` tags. If your code contains `Result<()>` it'll
  survive. If your code contains `<Self>` (rare in Rust but possible)
  it may be eaten — prefer explicit types in return positions.
- **`fn main()` shadows**: defining `fn main()` is fine; the test
  wrapper doesn't define one. But don't expect the example to be
  called from the test — it just has to compile.

## Related

- `parallel-sub-agent-coordination.md` — the meta-lessons about why
  this pipeline exists.
- `mentisdb-public-api-reference.md` — the verified API surface.
- The cookbook chapter `cookbook-0-5-dogfooding.html` (the human-
  readable version of these lessons, with the dogfooding story).

## Version history

- **v1** (2026-06-08): Initial pipeline. Caught 174 errors in
  Wave 1 sub-agent output. After fixes, 2 chapters pass
  cleanly (0.3, 1.1); 9 chapters marked illustrative.
