---
schema_version: 1
name: code-performance-reviewer
description: Use when reviewing Rust code for micro-optimizations and performance — hot-path audits, allocation/cache footprint review, API design for speed, Criterion-backed regressions, or flat profiles with no obvious hotspot. Inspired by Abseil Performance Hints (Jeff Dean & Sanjay Ghemawat). Pair with rust-backend-engineer (standards), code-review (general audit method), rust-criterion-benchmarking, and rust-concurrency-patterns.
tags: [rust, performance, micro-optimization, code-review, cache, allocations, criterion, hot-path, abseil]
triggers: [performance review, micro-optimize, hot path, allocation profile, cache miss, flat profile, speed up rust, reduce allocations, criterion, pprof, perf, SmallVec, zero-copy, bulk API]
---

# code-performance-reviewer

Use when reviewing Rust code for micro-optimizations and performance — hot-path audits, allocation/cache footprint review, API design for speed, Criterion-backed regressions, or flat profiles with no obvious hotspot. Inspired by Abseil Performance Hints (Jeff Dean & Sanjay Ghemawat). Pair with rust-backend-engineer (standards), code-review (general audit method), rust-criterion-benchmarking, and rust-concurrency-patterns.

# code-performance-reviewer

Disciplined performance review of Rust code through the lens of Jeff Dean &
Sanjay Ghemawat's **Performance Hints** (Abseil, 2025 —
https://abseil.io/fast/hints.html), translated to idiomatic Rust.

**Scope:** single-binary / library micro-performance — CPU, memory, cache,
allocations, locks, API shape. Not distributed systems or ML hardware tuning.

**Core principle (Knuth, full quote):** forget about small efficiencies ~97% of
the time — *but do not pass up the critical 3%*. A 12% easily-obtained gain is
never marginal in quality software. Prefer the faster alternative when it does
not significantly hurt readability.

**Evidence before claims.** Every finding cites `file:line`, states whether the
code is on a **hot path**, and proposes one concrete fix. Unmeasured
"this looks slow" off a cold path is Minor at most.

Companion skills:
- `code-review` — severity taxonomy, general method, security/reliability
- `rust-backend-engineer` — idioms, types, security, workflow
- `rust-criterion-benchmarking` — how to measure
- `rust-concurrency-patterns` — locks, DashMap, WAL, throughput

---

## When to engage this skill

- User asks for performance review, micro-optimizations, or "make it faster"
- Profiling / Criterion shows a hotspot or a flat profile after low-hanging fruit
- Designing library APIs that will be called from many call sites
- Pre-merge audit of hot request/scrape/parse/encode paths
- Investigating high RSS, allocator pressure, or cache-miss-bound code

**Do not** micro-optimize test-only code beyond asymptotic complexity, or
one-shot scripts. Do not sacrifice correctness or introduce dual mechanisms.

---

## Philosophy (from Abseil → Rust)

1. **Think about performance while writing**, not only after profiles go flat.
   Flat profiles mean cost is smeared everywhere — hard to start.
2. **Library code is high leverage.** Callers often cannot fix your internals.
   Choose good defaults (`SmallVec`, `&str`, bulk APIs) with low local complexity.
3. **Estimate before implementing.** Back-of-envelope with known costs; discard
   alternatives that cannot win.
4. **Measure before claiming.** Criterion for micro; `perf`/`samply` CPU
   profiles for systems; allocation profiles when the allocator is suspect.
5. **Many 1% wins compound.** Twenty small clean improvements beat one heroic
   rewrite — needs stable benches.
6. **Deep modules.** Keep public interfaces narrow so layout/alloc changes stay
   inside encapsulation boundaries.
7. **Don't pay for what you don't use.** Thread-safety, generality, stats, and
   logging on hot paths have real costs.

### Rough cost table (order-of-magnitude, modern x86)

| Operation | ~Cost |
|---|---|
| L1 reference | 0.5 ns |
| L2 reference | 3 ns |
| Branch mispredict | 5 ns |
| Uncontended mutex lock/unlock | 15 ns |
| Main memory reference | 50 ns |
| Compress 1KB (Snappy-class) | 1 µs |
| SSD 4KB read | 20 µs |
| Same-DC network RTT | 50 µs |
| 1MB sequential DRAM | 64 µs |
| 1MB over 100Gbps net | 100 µs |
| Disk seek | 5 ms |
| Transcontinental RTT | 150 ms |

Track *your* higher-level costs too: SQL point read, HTTP hop, HTML render,
serde of a typical payload. Without numbers you cannot estimate.

**Worked example (Abseil method, Rust framing):** scanning 1M strings for a
prefix. Data ~50MB ⇒ memory-bound floor ≈ 50MB / 16GB/s ≈ 3ms. If the compare
branch is data-dependent, ~50% of 1M branches mispredict ⇒ 0.5M × 5ns ≈ 2.5ms
extra. Conclusion: bandwidth and mispredicts are the same order — so (a) keep
the data compact (arena/`Box<str>`, not `Vec<String>` with capacity slack), and
(b) a SIMD/memchr-style scan that removes the branch can roughly halve the
time. That estimate decides the fix *before* writing any code.

---

## Review method

1. **Scope.** Whole crate, module, hot function, or diff? Hot path vs init/setup?
2. **Classify code**
   - Test-only → asymptotics + test runtime only
   - App-specific → is it per-request / per-item?
   - Library / multi-caller → apply techniques aggressively when cheap
3. **Estimate** dominant ops (allocs, copies, syscalls, locks, branches, bytes).
4. **Measure** when tradeoffs are non-obvious (Criterion; `perf record`/`samply`;
   `dhat` or alloc counters if available).
5. **Apply lenses below** in order: algorithmic → memory representation →
   allocations → avoid work → compiler help → code size → concurrency →
   serialization → Rust containers.
6. **Report** with severity, evidence, estimated impact class, one fix.
7. **Gate claims** with a bench or profile delta when Important+.

### Severity (performance-specific)

- **Critical** — pathological complexity on production path (O(n²) per request),
  unbounded memory (`posts_per_page => -1` class), lock held across network I/O,
  allocator thrash that OOMs / freezes host (this repo: uncapped `cargo test`
  froze the host 2026-07-29; FPM OOM 2026-06-23 — both real).
- **Important** — measurable hot-path waste: needless alloc/copy per request,
  wrong container, missing `reserve`, regex where prefix match suffices, missing
  bulk API forcing N crossings, contended global lock, false sharing.
- **Minor** — cold-path micro-opts, style-level `clone` cleanup, layout polish
  with tiny footprint, speculative SIMD. Batch only with approval.

### Impact classes (for findings)

- **Structural / algorithmic** — often 2–10×+
- **Representation / cache** — often 10–50% system-wide when data is large
- **Allocation reduction** — often 10–30% on alloc-heavy paths
- **Avoided work / fast path** — highly variable; can be huge
- **Lock / parallel** — throughput-bound multi-core wins
- **Death by 1000 cuts** — many 1% changes after profile is flat

---

## Lens 1 — Algorithmic improvements

Highest leverage. Prefer these over micro-opts.

Checklist:
- [ ] Wrong asymptotics? (nested loops, repeated full scans, sort when hash works)
- [ ] Sorted intersection → hash set lookup
- [ ] Ordered map used only for equality lookups → `HashMap`/`HashSet`
- [ ] Graph/build done incrementally with per-edge checks → one-shot construction
- [ ] Hash quality so expected O(1) is not accidentally O(n)

Rust notes:
- Hot maps with internal (non-adversarial) keys: swap the default SipHash
  hasher for `rustc_hash::FxHashMap` (rustc-hash 2.x) or `foldhash`
  (hashbrown 0.15's fast default). Keep SipHash for attacker-controlled keys.
- `HashMap::entry()` / `get_or_insert_with` instead of `contains_key` + `insert`
  (double hash + double lookup).
- `Vec::swap_remove` (O(1)) instead of `remove` (O(n)) when order doesn't matter.
- `sort_unstable` instead of `sort` when stability isn't required;
  `sort_by_cached_key` when the key is expensive to compute.
- `VecDeque` for ring-buffer / pop-front patterns, not `Vec::remove(0)`.
- Ordered lookups on moderate N: sorted `Vec` + `binary_search` often beats
  `BTreeMap` on cache behavior. `blart` (ART) for large ordered string keys.
- Build structures in bulk (`extend`, `from_iter`, `collect`) instead of
  repeated insert with rebalancing/rehash churn when input is known.

---

## Lens 2 — Better memory representation

Touch fewer cache lines; cut memory bus traffic (helps neighbors on the machine).

### Compact data structures

- Smaller integer types when domain fits (`u8`/`u16`/`u32` vs `usize`/`u64`)
- `#[repr(u8)]` enums instead of pointer-sized discriminants when it matters
- Bitflags / `bitvec` / manual bitsets instead of `HashSet<SmallId>`
- Dense arrays indexed by small IDs instead of maps
- `Box<str>` / `Box<[T]>` for immutable stored data (drops the capacity field,
  8 bytes per value) — `String::into_boxed_str`, `Vec::into_boxed_slice`
- `Arc<str>` / `Arc<[T]>` for shared immutable data — cheap clone, no double
  indirection of `Arc<String>`
- Enum size = largest variant + discriminant: `Box` large cold variants so the
  common path stays small

### Memory layout

- Field order: group by access (hot together); put cold fields last or behind
  `Box`/separate SoA arrays
- Separate hot read-mostly from hot mutable to reduce false sharing / invalidation
- Use `#[repr(C)]` + manual packing only inside well-tested modules; validate
  with benches — packing can hurt if it causes under-aligned hot loads
- Check sizes with `static_assertions::assert_eq_size!` in tests and
  `cargo +nightly rustc -- -Zprint-type-sizes` for full layout dumps
- `#[repr(align(64))]` for false-sharing fixes — easy to bloat; measure

### Indices instead of pointers

- On 64-bit, rich pointer graphs waste memory and scatter cache lines
- Store `u32` indices into an arena/`Vec<T>` when cardinality fits
- `slab` / `slotmap` (generational indices) are the canonical crates for this
- Contiguous `T[]` beats pointer soup for traversal

### Batched / flat storage

- Avoid per-element heap nodes (`LinkedList`, naive tree maps) on hot data
- Prefer `Vec`, slab/arena, chunked structures, flat hash maps
- Partition into fixed-size chunks when you need both locality and growth

### Inlined / small-vector storage

When N is usually small:
- `smallvec::SmallVec<[T; N]>` (spills to heap past N) / `arrayvec::ArrayVec`
  (fallible push past N) / `tinyvec`
- `compact_str` / `smartstring` for short strings (inline ≤ 24 bytes)
- Caveat: large `T` or large inline N bloats every instance (stack and struct size)

### Nested maps

- `HashMap<A, HashMap<B, V>>` → often `HashMap<(A,B), V>`; keep nested only if
  the outer key is huge and shared
- Measure both; Abseil notes nested can win when the first key is large

### Arenas

- `bumpalo`, `typed-arena`, or domain-specific `Vec`+indices for many
  short-lived related objects: fewer allocs, better locality, cheap bulk drop
- Caveat: long-lived arena holding short-lived junk → memory bloat
- Size the arena / pre-reserve when possible

### Arrays / bitsets instead of maps/sets

- Small integer or enum domain → `[V; N]`, `Vec<Option<V>>`, or bitset
- Set ops become word-parallel AND/OR

---

## Lens 3 — Reduce allocations

Alloc cost = allocator time + init/drop + **cache footprint** (each alloc tends
toward a new cache line in long-running programs).

Checklist:
- [ ] Avoid alloc when static/empty sentinel works (`&[]`, `Cow::Borrowed("")`)
- [ ] Prefer stack (`ArrayVec`, small arrays) when lifetime is scoped
- [ ] `Vec::with_capacity` / `reserve` when size known — never grow one-by-one
  in a loop without reserve
- [ ] Prefer `reserve` + `push` over `resize` + assign when element construct is dear
- [ ] Move (`std::mem::take`, `swap`, ownership transfer) instead of `clone`
- [ ] Store `&T` / indices / `Arc` in transient structures instead of deep clones
- [ ] Hoist buffers out of loops; reuse `Vec`/`String`/`BytesMut` with `clear`
- [ ] Caveat: reused buffers retain high-water capacity — periodically
  `shrink_to` / recreate every N uses if sizes vary wildly
- [ ] `Bytes`/`BytesMut` for shared I/O buffers; avoid copy into `Vec` then again
- [ ] Iterator pipelines without intermediate `collect` when a single pass works
- [ ] `Cow<'_, str>` / `Cow<'_, [T]>` when sometimes borrowed, sometimes owned
- [ ] `Rc` instead of `Arc` in provably single-threaded code (atomic refcount
      traffic is not free)

Size-hint nuance: `iter.map(f).collect::<Vec<_>>()` already reserves via
`size_hint`; `iter.filter(f).collect()` does **not** — add `with_capacity` or
`collect_into` a reused buffer when the filtered count is predictable.

Rust-specific smells (each is a finding when on a hot path):
- `.clone()` on `String`/`Vec`/`HashMap` in a per-item loop
- `format!` / `to_string()` just to pass to an API that takes `impl AsRef<str>`
- `format!` chains in a loop → `write!` into one reused `String`
  (`use std::fmt::Write`), `String::with_capacity`
- `to_lowercase() == other` → `eq_ignore_ascii_case` (no alloc, ASCII) — flag
  the Unicode caveat if input is non-ASCII
- `regex::Regex::new` inside a function body → compile once in `OnceLock`;
  better: `starts_with` / `split_once` / `memchr` when a regex isn't needed
- Building `PathBuf` repeatedly from the same prefix
- `serde_json::to_vec` + parse cycles when a typed value already exists
- Number formatting via `format!` on ultra-hot paths → `itoa` / `ryu`
- `s.as_bytes().to_vec()` where `s.into_bytes()` moves the allocation
- `Rc<RefCell<T>>`/`Arc<Mutex<T>>` for a plain counter → `Cell<u64>` / `AtomicU64`

### Allocator choice (system-level lever)

For alloc-heavy services, swapping the global allocator is often a bigger win
than any single code change: `mimalloc` or `tikv-jemallocator` via
`#[global_allocator]`. Measure on the real workload — gains are largest under
multi-threaded alloc churn; it also changes RSS behavior. One-line change,
benchmark before and after.

---

## Lens 4 — Avoid unnecessary work

Often the biggest win: don't do it.

### Fast paths for common cases

- Structure code so the common case is branch-predictable and allocation-free
- e.g. push when capacity remains; 1-byte varint; empty error-free stats skip
- Keep slow path cold (`#[cold]`, separate function) so it does not pollute I-cache

### Precompute once

- Expensive properties, lookup tables, fingerprints of large blobs
- Validate inputs at module boundaries; don't re-check deep inside
- `OnceLock` / `LazyLock` for process-wide immutable tables (regexes, maps)

### Hoist from loops

- Bounds, date formatting, config flags, logger enabled checks

### Defer

- Don't compute stats/sharding/subtrees until a consumer needs them

### Specialize

- Hot call site may not need full generality: `starts_with`/`split_once` vs
  full regex; `memchr`/`memchr::memmem` for byte/substring search;
  `aho-corasick` for many patterns at once; manual format vs `format!`

### Cache

- Fingerprint-keyed caches for large serialized inputs

### Logging / stats on hot paths

- Even disabled logging can cost a load+branch and block inlining/opts
- Precompute `log::log_enabled!` / a cached bool outside nested loops
- Compile-time level stripping: `log`/`tracing` `max_level_*` /
  `release_max_level_*` cargo features make disabled calls zero-cost
- Sample stats (1/N requests) instead of every event
- Counters: `AtomicU64` relaxed increments, not `Mutex<u64>`
- Drop useless counters entirely

---

## Lens 5 — Help the compiler

Only when profiles show pain — rustc/LLVM are often already good. Inspect
assembly (`cargo-show-asm`, `perf annotate`, Godbolt) for critical functions.

Techniques:
- Avoid `dyn Trait` on hottest loops when monomorphization is OK
- Split slow path into `#[cold] fn` / `#[inline(never)]` so hot path stays lean
- Iterate over slices instead of indexing (`for x in s`, `chunks_exact`) —
  elides bounds checks and enables autovectorization
- Copy small hot data into locals to end aliasing doubts (aids vectorization)
- Explicit SIMD: `std::simd` (nightly portable SIMD), `wide`, or `core::arch`
  intrinsics for bulk byte/number work (`packed_simd` is deprecated — don't
  recommend it)
- Process chunks of 4/8/16 items with `chunks_exact` when alignment allows
- Careful `#[inline]` — helps tiny hot getters; hurts when code size explodes
- `#[inline(never)]` on rare error paths
- Limit monomorphization blowup: convert type params to trait objects or
  function pointers for cold bulky code; keep hot kernels monomorphized

### Build flags (cheap, system-wide)

- `[profile.release] lto = "thin"` (or `"fat"` for the final binary),
  `codegen-units = 1`, `panic = "abort"` where acceptable
- `RUSTFLAGS="-C target-cpu=native"` on controlled deploy targets (unlocks
  autovectorization; not for distributed binaries)
- PGO via `cargo-pgo` for mature hot binaries — typically 5–15%
- `debug = 1` (line tables) in the release profile used for profiling, so
  `perf`/`samply` attribute correctly

---

## Lens 6 — Code size (I-cache / compile time)

Large code → longer builds, fatter binaries, I-cache pressure, worse predictors.
Especially important for **widely used generics and macros**.

- Measure monomorphization bloat with `cargo llvm-lines`; binary size with
  `cargo bloat`
- Trim code that ends up inlined at many call sites
- Avoid heavy `format!`/Display machinery in tiny inlined helpers
- Collapse repeated map-insert initialization into one `from`/`extend`
- Share non-generic bulky logic in a non-generic inner function
  (`fn inner(x: &str)` called from `fn api<S: AsRef<str>>(s: S)`)
- `impl AsRef<str>` on a widely-called API monomorphizes per caller;
  sometimes `&str` + one conversion at the edge is smaller
- Watch proc-macro / serde derives on huge types pulled into hot crates

---

## Lens 7 — Parallelization and synchronization

See also `rust-concurrency-patterns`.

- Parallelize independent items (`rayon` for CPU-bound, `tokio` task batches
  for I/O-bound) when spare cores/memory bandwidth exist — measure; bandwidth
  saturation can make parallel *slower*
- Amortize lock acquisition (one lock for a batch, not per item)
- Keep critical sections short — **never** hold a lock across await/RPC/disk
- Shard contended maps (`DashMap`) or use concurrent maps; be careful which
  hash bits pick the shard (don't skew the inner table)
- False sharing: `crossbeam_utils::CachePadded<T>` around per-thread counters
  instead of hand-rolled `#[repr(align(64))]`
- Counters/flags: atomics (`AtomicU64`, `Ordering::Relaxed`) instead of
  `Mutex<u64>`; `parking_lot` for smaller/faster sync locks in sync code
- Prefer bounded channels / batching over per-item task spawn (context switches)
- Lock-free only via proven structures (`crossbeam`, `arc-swap`) — not
  hand-rolled atomics unless expert + tested
- Thread-compatible (external sync) by default for library types; internal sync
  only when typical use needs it (so uncontended callers don't pay)

---

## Lens 8 — Serialization / protobuf-like data

Protobufs / heavy schema frameworks are convenient and **expensive**. Abseil
example: list of 1000 points ~20× faster as `Vec<Struct>` than protobuf.

Rust equivalents — beware of:
- `prost` / `protobuf` / large `serde` graphs on hot inner loops
- Deep message hierarchies and map fields
- Re-encoding/decoding the same blob repeatedly

Prefer:
- Plain structs + `Vec` on internal hot paths; serialize at boundaries
- `bytes::Bytes` to avoid copy of large fields
- Reuse parse buffers / message objects across loop iterations when APIs allow
- `simd-json` for proven hot JSON parsing (needs a mutable input buffer)
- `itoa` / `ryu` for fast number formatting
- Compact encodings (`rkyv`, flatbuffers, custom packed) only when measured need
- Avoid serde_json on hot paths when a binary format or manual parser suffices

---

## Lens 9 — Rust container & idiom cheat sheet (Abseil → crates)


### Containers

| Abseil / C++ idea | Rust default | Faster / denser alternatives |
|---|---|---|
| `std::vector` | `Vec<T>` | pre-`reserve`; `SmallVec`/`ArrayVec` if small |
| `absl::InlinedVector` | — | `smallvec`, `arrayvec`, `tinyvec` |
| `std::unordered_map` | `HashMap` (hashbrown) | `FxHashMap` (rustc-hash 2.x) / `foldhash` for internal keys |
| `absl::flat_hash_map` | hashbrown (open addressing) | same; good hasher; bulk insert; `entry` API |
| `std::map` | `BTreeMap` | sorted `Vec` + `binary_search`; `blart` for string keys |
| bit sets | `HashSet` of ids | `bitvec`, `fixedbitset`, integer bitmasks |
| `gtl::small_map` | — | `SmallVec` of pairs linear scan; `heapless::FnvIndexMap` |
| intrusive list | `LinkedList` (rare) | `intrusive-collections`; usually better as `Vec`+indices |
| arenas | — | `bumpalo`, `typed-arena`, `slab`, `slotmap` |
| indices vs pointers | — | `slab` / `slotmap` with `u32` keys |

### Strings & bytes

| Need | Reach for |
|---|---|
| Shared immutable string | `Arc<str>` (not `Arc<String>`) |
| Stored immutable string | `Box<str>` via `into_boxed_str` |
| Short strings | `compact_str`, `smartstring` |
| Conditional ownership | `Cow<'_, str>` |
| Substring / byte search | `memchr`, `memchr::memmem` |
| Many patterns | `aho-corasick` |
| Shared I/O buffers | `bytes::Bytes` / `BytesMut` |
| Number formatting | `itoa`, `ryu` |

### Sync & misc

| Abseil / C++ idea | Rust default | Alternative |
|---|---|---|
| `string_view` / `Span` | `&str`, `&[T]` | always prefer views in APIs |
| `FunctionRef` | `impl Fn` / `fn()` | avoid `Box<dyn Fn>` on hot calls |
| `alignas(64)` false sharing | — | `crossbeam_utils::CachePadded` |
| faster mutex | `std::sync::Mutex` | `parking_lot` (sync code) |
| stats counters | `Mutex<u64>` | `AtomicU64` (relaxed) |
| `Status`/`StatusOr` tax | `Result<T,E>` | on ultra-hot infallible paths, don't force `Result` |
| allocator | system malloc | `mimalloc` / `tikv-jemallocator` |

API design (Abseil "API considerations"):
- **Bulk APIs** — `lookup_many`, `delete_many`, batch encode/decode; amortize
  locking and boundary costs. If callers can't change, bulk internally + cache.
- **View types** — accept `&str`, `&[T]`, `impl AsRef<Path>`, not owned `String`
  unless transferring ownership.
- **Pre-allocated / precomputed args** — let callers pass clocks, buffers,
  scratch space they already have.
- **Thread-compatible vs thread-safe** — default externally synchronized.

---

## Flat profile playbook

When CPU profile has no tall towers:

1. Take the many 1% wins (layout, reserve, hoist, cold paths) with benches
2. Flame graph: find loops near the top of stacks; restructure whole loop
3. Step up a level — structural/API changes beat instruction tweaks
4. Replace overly general code (regex → prefix; serde → manual; generic → special)
5. Allocation profile — cut top alloc sites (allocator time + cache)
6. Hardware counters — cache misses, branch misses (`perf stat`, `perf mem`)
7. I-cache: check binary growth and over-inlining (`cargo bloat`, `cargo llvm-lines`)

---

## Measurement toolkit (Rust)

| Need | Tool |
|---|---|
| Microbench | `criterion` (see rust-criterion-benchmarking); `divan` as lighter alternative |
| CI-stable regression gate | `iai-callgrind` (instruction counts, noise-immune) |
| CPU profile | `samply record`, `perf record` + `cargo flamegraph` |
| Assembly | `cargo-show-asm`, Godbolt |
| Allocations | `dhat`, `heaptrack` |
| Cache/branch | `perf stat -e cache-misses,branches,branch-misses` |
| Lock contention (async) | `tokio-console` |
| Binary size | `cargo bloat` (`twiggy` is wasm-only) |
| Monomorphization bloat | `cargo llvm-lines` |
| Type layouts | `cargo +nightly rustc -- -Zprint-type-sizes` |
| PGO | `cargo-pgo` |

Rules:
- Bench with `--release` (and same RUSTFLAGS as prod)
- `black_box` inputs/outputs
- Prefer throughput metrics for batch work
- Distrust single-run deltas; use Criterion CI and baselines
- Microbenches can lie — validate important wins on realistic workloads

**Production host constraint (diariobitcoin):** never uncapped
`cargo test`/`cargo build` on the live box — use `nice -n 19` + `--jobs 4`,
prefer a dev machine. See AGENTS.md.

---

## Review output format

```markdown

## Performance review: <scope>


### Summary

- Hot paths identified: ...
- Profile/bench evidence: ... (or "static review only")
- Top opportunities: ...

### Findings


#### [Critical|Important|Minor] <title>

- **Where:** `path/file.rs:LINE`
- **Evidence:** quote + why hot
- **Mechanism:** (alloc / cache / algorithm / lock / work-avoidance / ...)
- **Impact class:** structural | representation | allocation | ...
- **Fix:** minimal concrete change (code sketch OK)
- **Validate:** criterion case / perf command / reason estimate suffices

### Non-findings / accepted costs

- ... (generality, readability tradeoffs explicitly OK)

### Suggested bench plan

- ...
```

Push back on premature micro-opts that hurt clarity with no hot-path evidence.
Push back on "optimize everything" — prioritize the critical 3%.

---

## Quick hot-path checklist (print this mentally on every file)

1. Per-item or per-request? If no → deprioritize.
2. Any alloc/copy that could be borrow, move, or reuse?
3. Any `HashMap`/`BTreeMap`/`Regex`/`format!` that could be simpler/denser?
4. Capacity reserved? Buffers hoisted out of loops? `entry` not double lookup?
5. Locks: scope minimal? sharded? held over I/O? `AtomicU64` for counters?
6. API: bulk + views possible without breaking encapsulation?
7. Logging/stats on the path — compile-time levels, sampling, hoisted checks?
8. Layout: hot fields together; cold behind `Box`; enums not bloated by cold variants?
9. Compiler: cold paths marked; inlining not exploding size; iterators not indexing?
10. Measured or estimated before recommending Important+ changes?

---

## Source attribution

Principles and structure adapted from:

> Jeffrey Dean & Sanjay Ghemawat, *Performance Hints*, 2025,
> https://abseil.io/fast/hints.html

Rust mappings, severity taxonomy, and review workflow are project-specific
for agent use in this workspace and related Rust codebases.