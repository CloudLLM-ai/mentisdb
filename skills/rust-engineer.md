---
name: rust-engineer
description: >
  MentisDB Rust engineering standards for writing, reviewing, and fixing
  production code. Use whenever implementing features, fixing bugs, adding
  APIs, or reviewing Rust changes in this repository. For shared-state and
  lock design, also load rust-concurrency-patterns. For Criterion benches,
  also load rust-criterion-benchmarking. Triggers: rust code, bug fix,
  implement, refactor, clippy, add test, public API, review rust.
triggers:
  - rust engineer
  - rust coding style
  - implement rust
  - fix rust bug
  - clippy
  - public API
  - write tests
  - review rust
---

# MentisDB Rust Engineer Skill

Load this skill before writing or reviewing Rust in this repository.
It encodes the house style observed in `src/` and the bar in `AGENTS.md`.

## Companion skills (load when relevant)

| Skill | When |
|-------|------|
| [`rust-concurrency-patterns.md`](rust-concurrency-patterns.md) | Shared mutable state, DashMap/RwLock, lock contention, write batching, WAL, async lock scope |
| [`rust-criterion-benchmarking.md`](rust-criterion-benchmarking.md) | Writing or interpreting `cargo bench` / Criterion runs, throughput vs latency, baselines |

Do not re-derive concurrency or bench conventions from memory — open the
companion skill and follow it.

## Non-negotiables

1. **Warning-free builds.** `make clippy` is the gate:
   ```bash
   make clippy   # cargo fmt + clippy --all-targets --all-features -- -D warnings
   cargo test --all-features
   ```
   Treat every Clippy warning as a hard error. Do not paper over with `#[allow]`
   unless the reason is documented and unavoidable.

2. **Tests live outside production modules.** Prefer `tests/*.rs` integration
   and regression tests over `#[cfg(test)]` modules in `src/`. Keeping tests
   out of library code keeps crates portable, docs cleaner, and public APIs
   honest. Small pure unit tests next to private helpers are acceptable only
   when they cannot be expressed through the public surface.

3. **Every bug fix needs a regression test.** Reproduce the failure first,
   then fix, then prove the test fails without the fix and passes with it.

4. **Public surface is a manual.** Every exported type, function, trait, enum
   variant, and struct field gets rustdoc. Prefer short prose plus a runnable
   `# Examples` block over long narrative. Document errors, invariants, and
   when *not* to use the API.

5. **Parameters and types are correct by construction.** Prefer enums and
   newtypes over free-form strings. Validate at construction time (see
   `BearerTokenScope::chain`, `BearerTokenError` variants). Fail early with
   specific error enums, not opaque `String` errors.

6. **Update `changelog.txt` when the work is done.** Every completed fix,
   feature, test suite, performance change, or user-visible docs change that
   ships with the crate must land an entry under the **most recent (top)
   version block** in `changelog.txt` (usually the `UNRELEASED` section at
   the top of the file). Do not invent a new version header unless you are
   cutting a release. After adding your entry, **re-sort all entries under
   that top version block by impact**: most important first, trivial last.
   Typical order of importance:

   1. Security / data-loss / integrity breaks
   2. User-facing bugs (auth, MCP, REST, dashboard, CLI)
   3. Features and API surface changes
   4. Performance and scalability
   5. Tests, refactors, internal cleanups
   6. Docs-only and chore

   Match existing style: bullet lines with a short prefix such as
   `fix(scope):`, `feat(scope):`, `perf(scope):`, `docs(scope):`,
   `test(scope):`, `refactor(scope):`, then a clear one- or two-line
   description of *what changed and why it matters*.

## Design bias (KISS + robust)

- **Keep it simple and stupid.** Small functions, obvious control flow, no
  clever macros unless they remove real duplication.
- **Code robust parts that are used together.** Co-locate related types,
  validation, and persistence. Prefer one coherent module over scattered
  helpers.
- **Be intentional with memory.** Avoid needless clones and large temporary
  allocations on hot paths. Prefer borrowing and streaming where it stays
  readable. Durability and correctness beat micro-optimizations — measure
  before optimizing (`rust-criterion-benchmarking`).
- **Enums over strings** for protocol actions, scopes, thought types, roles,
  storage adapters, and auth targets.
- **Append-only and integrity first.** Persistence changes must preserve
  hash-chain and explicit integrity checks.
- **No `cloudllm` dependency** from `mentisdb`. This crate is standalone;
  `cloudllm` may depend on us, never the reverse.
- **Comment only what is not obvious.** Prefer better names and types over
  narrating code. Comment *why* for security, concurrency, compatibility, or
  surprising invariants.
- **Lock granularity over cleverness.** Every shared lock is a serialization
  point. Prefer per-entity locks + DashMap over a global `RwLock<HashMap<…>>`.
  Details: `rust-concurrency-patterns`.

## Concurrency quick rules (full skill: companion)

Summary only — open `rust-concurrency-patterns.md` for patterns and code.

1. **DashMap** for multi-key maps; **per-entity `Arc<RwLock<T>>`** for values.
2. **Never hold a write lock across await** if avoidable; clone snapshot, process, re-acquire.
3. **Never hold a DashMap entry guard across await** — clone the `Arc` out first.
4. **`tokio::sync::*` locks in async code**, not `std::sync::Mutex` held across `.await`.
5. **Write buffering / auto_flush** is an operator tradeoff; flush on `Drop`.
6. WAL or batch flush only when measured need exceeds fsync limits; keep integrity.

## Benchmarking quick rules (full skill: companion)

Summary only — open `rust-criterion-benchmarking.md` for setup and gotchas.

1. Benches live under `benches/` with `harness = false`; tests stay in `tests/`.
2. Always `black_box` results; use `Throughput::Elements` for batches.
3. Capture with `cargo bench 2>&1 | tee …` or `make bench` — bare `>` swallows Criterion.
4. Compare **absolute medians** across meaningful runs; Criterion “regressed” labels drift with load.
5. HTTP load tests are custom Tokio harnesses, not pure Criterion micro-benches.

## Documentation style

Match existing modules (`src/auth.rs`, `src/lib.rs`, `src/webhooks.rs`):

```rust
//! Module-level overview: what this is, when to use it, storage model.

/// One-line summary of the type or function.
///
/// Longer explanation of policy, invariants, and failure modes.
///
/// # Examples
///
/// ```
/// use mentisdb::auth::BearerTokenScope;
///
/// let scoped = BearerTokenScope::chain("alice")?;
/// assert!(scoped.allows_chain("alice"));
/// # Ok::<(), mentisdb::auth::BearerTokenError>(())
/// ```
pub struct Example {
    /// Human-readable field documentation.
    pub alias: String,
}
```

- Document every public field.
- Document error variants with operator-facing meaning.
- Keep examples compiling under `cargo test --doc` where possible.

## Implementation checklist

When changing code:

| Step | Action |
|------|--------|
| 1 | Read neighboring modules for naming, error, and layout patterns |
| 2 | Prefer extending an existing type over adding a parallel abstraction |
| 3 | Use `Result` + domain error enums; implement `Display` + `Error` |
| 4 | Keep secrets hashed / constant-time compared; never log raw tokens |
| 5 | For shared state: check lock granularity (`rust-concurrency-patterns`) |
| 6 | Add or update tests under `tests/` for the behavior change |
| 7 | Run `make clippy` and relevant `cargo test` filters |
| 8 | If hot-path performance changed, add or update a Criterion bench |
| 9 | Update rustdoc if the public contract changed |
| 10 | Update `changelog.txt` under the **top** version block; re-sort that block by impact (most important first) |

## Testing conventions

- Integration tests under `tests/` with clear names:
  `standard_mcp_router_enforces_chain_scoped_bearer_tokens`.
- Use temp dirs and unique paths; always clean up.
- Cover success path, denial path, and upgrade/compat cases for wire formats.
- For auth/security: test missing token, wrong token, wrong scope, revoked
  token, and the happy path with the *minimal* privilege that should work.
- Do not rely on env pollution; lock process-global env when needed.
- Performance claims need numbers from Criterion (or an HTTP harness), not vibes.

## Review lens (use when auditing a bug)

1. **Correctness:** Is the happy path correct for the least-privilege caller?
2. **Fail modes:** Do error messages distinguish missing auth vs wrong scope?
3. **Defaults:** What happens when optional parameters are omitted?
4. **Scope leakage:** Can a scoped credential touch other chains via nested
   fields, default chain keys, or global-only tool misclassification?
5. **Regression surface:** Is there a test that would have caught this?
6. **Concurrency:** Is a global lock on the hot path? Hold time across await?
7. **Simplicity:** Is there a smaller fix that preserves the security model?
8. **Changelog:** Does the top version block in `changelog.txt` describe this
   change, and are its bullets ordered by impact?

## Changelog discipline

When a fix, feature, test, or other shippable change is complete:

1. Open `changelog.txt`.
2. Edit only the **latest (top) version section** — do not bury notes under older
   released versions.
3. Add a bullet that an operator or release-note reader can understand without
   reading the diff.
4. **Sort the entire top section** so the highest-impact items sit at the top
   and trivial items sit at the bottom. Re-order sibling bullets if your new
   entry outranks ones already there.
5. Prefer impact over chronology: a security fix always outranks a docs tweak
   even if the docs tweak landed later in the same unreleased cycle.

## Anti-patterns

- Stringly-typed scopes, roles, or tool policies where an enum fits
- Silent auth fall-through to `GlobalOnly` without documenting why
- Putting large test suites inside `src/` modules
- `unwrap()` on daemon request paths (prefer `Result` / HTTP status mapping)
- Vague 401 bodies that say "token required" when a valid token was presented
  but scope did not match
- `Arc<RwLock<HashMap<…>>>` for multi-key concurrent maps (use DashMap)
- `std::sync::Mutex` held across `.await`
- DashMap entry / shard guard held across `.await`
- Write buffers without `Drop` flush
- Drive-by refactors unrelated to the task
- `#[allow(dead_code)]` or `#[allow(clippy::…)]` without a written reason
- Claiming a performance win without a bench or load number
- Shipping a fix/feature/test without a `changelog.txt` entry under the top
  version
- Appending changelog bullets in random or pure chronological order instead of
  sorting the top section by impact
- Writing changelog lines that only restate the commit subject without saying
  what broke or what the user gains

## Version history

- **2026-07-09** — Initial skill from repository house style and AGENTS.md.
- **2026-07-09** — Enriched with concurrency and Criterion companions
  (`rust-concurrency-patterns`, `rust-criterion-benchmarking`); quick rules,
  checklist steps, review lens, and anti-patterns expanded.
- **2026-07-09** — Changelog discipline: always update top version in
  `changelog.txt` when work lands; sort that section by impact (most important
  first).
