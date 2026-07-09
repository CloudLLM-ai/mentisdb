---
schema_version: 1
name: rust-criterion-benchmarking
description: >
  Use this skill when writing, running, or interpreting Criterion benchmarks
  for Rust projects. Covers benchmark structure, group/throughput patterns,
  baseline management, output capture, and common gotchas.
tags:
  - rust
  - benchmarks
  - criterion
  - performance
  - testing
triggers:
  - benchmark
  - cargo bench
  - criterion
  - throughput
  - latency
  - performance measurement
---

# rust-criterion-benchmarking

Use this skill when writing, running, or interpreting Criterion benchmarks for
Rust projects. Covers benchmark structure, group/throughput patterns, baseline
management, output capture, and common gotchas.

Companion to `rust-engineer.md`. Prefer that skill for general style; load this
one when measuring performance. In MentisDB, `make bench` runs Criterion benches
and tees output — see `Makefile`.

# Rust Criterion Benchmarking

Use Criterion to measure real performance of Rust code with statistical rigor.
Never rely on intuition — let the numbers decide.

## Project Setup

```toml
# Cargo.toml

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
tempfile = "3"   # for isolated filesystem fixtures

[[bench]]
name = "my_bench"
harness = false   # REQUIRED — disables the default test harness
```

## File Structure

```text
benches/
  my_bench.rs    # harness = false entry point
  another.rs
tests/           # unit/integration tests go here, NOT in benches/
```

## Benchmark File Skeleton

```rust
//! Module-level doc: what this file benchmarks and how to run it.
//!
//! Run: cargo bench --bench my_bench
//! HTML report: target/criterion/my_bench/report/index.html

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn bench_single_op(c: &mut Criterion) {
    c.bench_function("my_op", |b| {
        b.iter(|| {
            // call the thing you're measuring
            black_box(my_expensive_fn(42));
        });
    });
}

fn bench_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_throughput");
    for size in [1usize, 10, 100, 1_000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("process", size), &size, |b, &n| {
            b.iter(|| {
                for _ in 0..n {
                    black_box(my_expensive_fn(n));
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_single_op, bench_batch);
criterion_main!(benches);
```

## Running

```bash
# All benches in the crate
cargo bench

# MentisDB convenience target (tees to /tmp)
make bench

# One bench file
cargo bench --bench my_bench

# One group or function (regex filter)
cargo bench --bench my_bench -- batch_throughput
cargo bench --bench my_bench -- "batch_throughput/process/100"

# Capture output (IMPORTANT: use tee, not redirect — see Gotchas)
cargo bench 2>&1 | tee /tmp/bench_results.txt
```

## HTML Report

Criterion always writes an HTML report at:

```text
target/criterion/<bench_name>/report/index.html
```

Open in a browser for flame graphs, violin plots, and regression comparisons.
This is generated even if stdout is swallowed.

## Latency vs. Throughput

Use `Throughput::Elements(n)` when benchmarking batch operations — Criterion
will report elem/s, not just wall time:

```rust
group.throughput(Throughput::Elements(batch_size as u64));
```

For single operations, `bench_function` is sufficient.

## Fixtures and Isolation

Always use `tempfile::TempDir` for disk-backed fixtures:

```rust
use tempfile::TempDir;

fn make_fixture() -> (MyStore, TempDir) {
    let dir = tempfile::Builder::new().prefix("bench-").tempdir().unwrap();
    let store = MyStore::open(dir.path()).unwrap();
    (store, dir) // return dir to keep alive — drop = delete
}
```

Use `BatchSize::SmallInput` or `BatchSize::LargeInput` when the fixture must be
created fresh per iteration:

```rust
b.iter_batched(
    || make_fixture(),
    |(mut store, _dir)| black_box(store.write(42)),
    criterion::BatchSize::SmallInput,
);
```

## Interpreting Results

```text
my_op   time: [1.234 µs  1.250 µs  1.267 µs]
        change: [-3.2% -1.5% +0.1%]  (p = 0.12 > 0.05)
        No change in performance detected.
```

- Three numbers = lower/median/upper of the confidence interval
- `change`: relative delta vs. the stored baseline
- `p > 0.05` → not statistically significant; treat as noise
- **"Performance has regressed"** ≠ code is broken — Criterion is comparing
  against the last run's baseline, which may have been on an idle machine

**Golden rule: compare absolute median numbers across meaningful runs, not just
the Criterion delta label.**

## Gotchas

1. **Output capture**: `cargo bench > file.txt` swallows Criterion stdout. Use
   `cargo bench 2>&1 | tee file.txt`.
2. **Baseline drift**: Two runs on the same code at different system loads will
   show "regression." Tag real baselines in MentisDB or a file.
3. **`black_box`**: Always wrap return values in `black_box()` to prevent the
   compiler from optimizing away the work being benchmarked.
4. **`harness = false`**: Without this in `Cargo.toml`, the bench file will not
   compile as a benchmark.
5. **Temp dir lifetime**: If you drop `TempDir` before the benchmark loop, the
   directory is deleted mid-bench. Keep it in scope.
6. **HTTP benches**: Criterion is designed for in-process micro-benchmarks. For
   HTTP load testing (concurrent requests, p99 latency), write a harness-free
   `[[bench]]` binary with Tokio tasks and `reqwest` instead (see
   `benches/http_concurrency.rs` in this repo).

## Version history

- **2026-07-09** — Initial skill; linked from `rust-engineer.md`.
