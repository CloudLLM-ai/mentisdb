---
name: rust-fuzzing
description: >
  Coverage-guided fuzzing for Rust crates with cargo-fuzz (libFuzzer) and
  `arbitrary`: choosing targets, writing harnesses that assert real
  invariants (round-trip, differential, model-based), corpus and crash
  handling, turning every finding into a stable regression test, Makefile
  targets and CI. Shared across the workspace's Rust projects (mentischat,
  MentisDB, cloudllm, DiarioBitcoin). Load with rust-engineer whenever code
  parses untrusted bytes or a state machine is driven by external input.
  Triggers: fuzz, fuzzing, cargo fuzz, libfuzzer, arbitrary, fuzz target,
  corpus, crash input, parser hardening, untrusted input.
triggers:
  - fuzz
  - fuzzing
  - cargo fuzz
  - libfuzzer
  - arbitrary
  - fuzz target
  - corpus
  - crash input
  - parser hardening
  - untrusted input
---

# Rust Fuzzing Skill

Tests prove the cases you thought of. Fuzzing finds the ones you did not:
the panic on a 0-byte body, the `usize` overflow on a hostile length
prefix, the state machine that loses a message after
`join → leave → join`. Every crate that parses bytes it did not produce, or
exposes an API that outside callers drive in arbitrary order, gets fuzz
targets. Every finding becomes an ordinary regression test.

## Non-negotiables

1. **Fuzz every trust boundary.** A function that takes bytes, strings, or
   JSON from a socket, a file on disk, a CLI argument, an environment
   variable, or another process gets a target: name and id validators,
   wire/body decoders, snapshot and file loaders, query/cursor parsers,
   frame decoders, and config parsers.
2. **Assert an invariant, not just "no panic".** "No panic" is the floor.
   Each target also checks at least one property: round-trip, a size or
   resource bound, agreement with a reference model, or a typed error
   instead of a crash (see *Invariants*).
3. **Every crash becomes a regression test under `tests/`.** Minimize the
   input (`cargo fuzz tmin`) and paste it as a byte literal into a named
   integration test that runs on stable. Prove the test fails before the fix
   and passes after it. Add the input to the corpus. Write a changelog line.
   The fuzz job is how bugs are found; `cargo test` is what keeps them fixed.
4. **The harness goes through the public API.** The fuzz crate is a separate
   package and sees only `pub` items, the same rule as `tests/`. If a parser
   must be fuzzed directly, make it `pub` and document it. Never add
   `#[cfg(test)]` or fuzz-only escape hatches.
5. **Fuzzing never touches the stable gates.** The fuzz crate needs nightly
   and lives outside the main package and workspace. `cargo check
   --all-targets`, clippy, the MSRV job, and `cargo publish` must not see it.

## Layout

```
fuzz/
  Cargo.toml              # package <crate>-fuzz, publish = false, own [workspace]
  fuzz_targets/
    names.rs              # one file per target, named for what it fuzzes
    snapshot_restore.rs
    bus_ops.rs
  corpus/<target>/        # committed seeds plus cmin-minimized corpus
  artifacts/<target>/     # crashes (gitignored; copy them into tests/)
  dicts/<target>.dict     # optional libFuzzer dictionary (JSON keys, tokens)
```

`fuzz/Cargo.toml`:

```toml
[package]
name = "mycrate-fuzz"
version = "0.0.0"
publish = false
edition = "2021"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
arbitrary = { version = "1", features = ["derive"] }
mycrate = { path = ".." }

# Its own workspace: keeps the fuzz crate out of the parent package's and
# workspace's builds, clippy runs, MSRV checks and published tarball.
[workspace]
members = ["."]

[[bin]]
name = "names"
path = "fuzz_targets/names.rs"
test = false
doc = false
bench = false
```

- In a Cargo workspace (for example `diariobitcoin.com/rust`), give each
  crate that needs one its own `fuzz/` directory with its own `[workspace]`
  table, or put one `fuzz/` at the workspace root and add
  `exclude = ["fuzz"]` to the root `[workspace]`.
- Add `fuzz/target`, `fuzz/artifacts` and `fuzz/coverage` to `.gitignore`.
- Add `fuzz/` to `exclude` in the published crate's `[package]` so the
  corpus does not ship to crates.io.

## Writing a target

Raw bytes, for parsers:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use mycrate::names::AgentName;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    if let Ok(name) = s.parse::<AgentName>() {
        // Round-trip: what we accept, we print and accept again unchanged.
        let again: AgentName = name.to_string().parse().expect("round-trip");
        assert_eq!(name, again);
        assert!(name.as_str().len() <= mycrate::names::MAX_NAME_LEN);
    }
});
```

Structured input with `arbitrary`, for APIs and state machines. This is
model-based fuzzing: compare the real thing with a reference model simple
enough to be obviously correct.

```rust
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
enum Op {
    Register(u8),          // small ids → many collisions, more interesting states
    Send { from: u8, to: u8, body: String },
    Pull { agent: u8, limit: u8 },
    Deregister(u8),
}

fuzz_target!(|ops: Vec<Op>| {
    let real = mycrate::Bus::in_memory();
    let mut model = Model::default(); // HashMap<u8, VecDeque<String>>, ~50 lines
    for op in ops.iter().take(256) {    // bound the work per input
        let got = apply_real(&real, op);
        let want = model.apply(op);
        assert_eq!(got, want, "diverged at {op:?}");
    }
    real.check_invariants(); // counters equal what the logs hold, etc.
});
```

Rules for harnesses:

- **Deterministic.** No wall clock, no randomness, no threads racing, no
  network, no sleeps. Inject a clock if the code reads time. A crash that
  does not reproduce is worth nothing.
- **No state shared between iterations.** Build fresh state per input. If
  setup is expensive (a Tokio runtime), build it once in a `OnceLock`. Keep
  nothing else that carries over between inputs.
- **Fast.** Aim for thousands of executions per second. Cap loop counts,
  collection sizes and `-max_len`. A slow target finds nothing.
- **Async code.** Fuzz the synchronous core where possible. Otherwise use
  a `current_thread` runtime from a `OnceLock` and `block_on`. A
  multi-threaded runtime makes runs non-reproducible.
- **Disk.** Load from bytes (`from_slice` / `from_reader` on a `&[u8]`)
  rather than files. If the API only takes a path, write into one
  `tempfile` directory created once and overwritten each iteration.
- **Avoid `cfg(fuzzing)`.** cargo-fuzz sets `--cfg fuzzing`. Use it only to
  skip checks the fuzzer cannot satisfy, such as signatures and checksums,
  and say why in a comment. Any other behaviour change means you are
  fuzzing a different program.

## Invariants worth asserting

| Kind | Example |
|------|---------|
| Round-trip | `parse(display(x)) == x`; `deserialize(serialize(v)) == v` |
| Typed rejection | Bad input yields `Err(SpecificError)`, never a panic or `Ok` with garbage |
| Bounds | Accepted names ≤ max length; a page ≤ its byte cap; memory charged ≤ budget |
| Differential | New parser agrees with the old one or with `serde_json`; fast path agrees with slow path |
| Model-based | Op sequences give the same observable results as a naive reference model |
| Conservation | Messages sent = delivered + pending + evicted; counters equal the contents of the structures |
| Exactly-once | No message id is delivered twice; no ack covers an id that was never sent |
| Idempotence | `normalize(normalize(x)) == normalize(x)`; restoring a snapshot twice equals restoring it once |

## Running

```bash
cargo install cargo-fuzz                      # once
rustup toolchain install nightly --profile minimal

cargo +nightly fuzz list
cargo +nightly fuzz build                     # all targets compile (CI does this)
cargo +nightly fuzz run names -- -max_total_time=60
cargo +nightly fuzz run names fuzz/corpus/names -- -runs=0   # replay corpus only
cargo +nightly fuzz run bus_ops -- -max_len=4096 -rss_limit_mb=2048
cargo +nightly fuzz run names -j8 -- -max_total_time=600     # parallel jobs
cargo +nightly fuzz run json_body -- -dict=fuzz/dicts/json_body.dict
```

- Builds are optimized with debug assertions and overflow checks **on**. An
  overflow panic in a fuzz run is a real finding even if release builds
  wrap silently.
- The default sanitizer is AddressSanitizer. For crates with no `unsafe`,
  `--sanitizer none` runs about 2× faster and loses nothing. Keep ASan
  where `unsafe` or FFI is involved.
- Seed each corpus with real inputs: the payloads the integration tests
  already use, captured wire bodies, a valid snapshot file. Coverage
  grows much faster from realistic seeds than from nothing.
- Measure what the fuzzer reaches with `cargo +nightly fuzz coverage
  <target>`. If a branch you care about is never hit, the target or the
  seeds are wrong.

## Handling a finding

1. Reproduce: `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-…`.
2. Minimize: `cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/crash-…`.
3. Print it as bytes. `cargo fuzz fmt <target> <file>` shows the
   `Arbitrary` value for structured targets.
4. Write the regression test in `tests/` on stable, named for the behaviour
   (`a_name_of_only_combining_marks_is_rejected`, not `fuzz_crash_3`), with
   the input as a literal.
5. Fix the root cause everywhere the pattern occurs, not just this input.
6. Add the minimized input to `fuzz/corpus/<target>/`, add a changelog
   line, and commit the test, fix and corpus file together.
7. After a long run, shrink the corpus before committing:
   `cargo +nightly fuzz cmin <target>`. Keep the committed corpus small
   (a few MB per repo).

## Makefile and CI

```make
FUZZ_SECONDS?=60
fuzz-build: ## Build every fuzz target (needs nightly and cargo-fuzz)
	cargo +nightly fuzz build
fuzz-replay: ## Replay each committed corpus once (fast regression pass)
	for t in $$(cargo +nightly fuzz list); do cargo +nightly fuzz run $$t fuzz/corpus/$$t -- -runs=0 || exit 1; done
fuzz: ## Fuzz TARGET for FUZZ_SECONDS (default 60)
	cargo +nightly fuzz run $(TARGET) -- -max_total_time=$(FUZZ_SECONDS)
```

- On every push and PR, run a CI job on nightly that does `fuzz-build`
  and `fuzz-replay`. It takes seconds and catches harness rot and
  regressions on old findings.
- Run time-boxed fuzzing (for example 5 minutes per target) as a
  separate workflow on `schedule` and `workflow_dispatch`, not on every
  push. Upload `fuzz/artifacts` when the job fails.
- Do not wire fuzzing into `make ci`, `make test` or the MSRV job. Those
  stay stable-only.

## Fuzzing, proptest and tests

- **Integration tests** pin behaviour you know about. They run on stable,
  every time.
- **proptest** (`tests/`, stable) states properties over generated
  inputs and shrinks failures. Use it for algebraic properties of pure
  functions where coverage guidance adds little.
- **cargo-fuzz** is coverage-guided and runs for minutes to hours. It
  finds inputs that reach deep branches. Use it for parsers, decoders
  and operation sequences.

Don't write the same property in both proptest and a fuzz target. Pick the
one that fits.

## Review lens

1. Does every new parser, decoder or loader of external input have a
   target, or a written reason why not?
2. Does the target assert something beyond "no panic"?
3. Is the harness deterministic, bounded and fast?
4. Is each past finding pinned by a stable test in `tests/`?
5. Do the stable gates (check, clippy, MSRV, publish) still ignore `fuzz/`?

## Anti-patterns

- Fuzz targets that only call the function and assert nothing
- A crash fixed without a regression test, so only the corpus remembers it
- Harnesses that read the clock, spawn threads or hit the network
  (non-reproducible)
- Fuzz-only `pub` shims or `cfg(fuzzing)` branches that change behaviour
- Committing `fuzz/artifacts/` or an unminimized multi-hundred-MB corpus
- Putting the fuzz crate in the main workspace, which breaks stable CI and
  MSRV
- Unbounded `Vec<Op>` loops, where one input runs for minutes and the
  fuzzer stalls
- Running long fuzz campaigns on every push, which slows CI without
  finding more

## Version history

- **2026-10-30**: Initial shared skill (cargo-fuzz, `arbitrary`,
  model-based targets, invariants, findings into stable regression tests,
  Makefile and CI layout), written alongside the first fuzz targets in
  mentischat.
