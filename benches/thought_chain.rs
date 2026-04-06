//! Benchmarks for MentisDb thought chain append, query, and traversal performance.
//!
//! Six benchmark groups cover the core in-process chain hot paths:
//!
//! - **`append_throughput`**: single-thought latency and batches of 10 / 100 / 1 000
//!   thoughts, reporting elements-per-second throughput.
//! - **`append_latency_percentiles`**: p50 / p95 / p99 tail-latency reporting for
//!   individual appends and batch-of-100 appends, printed via `eprintln!` since
//!   Criterion's built-in output only shows mean ± stddev.
//! - **`query_latency`**: indexed (`thought_type`, `tag`) and linear-scan (`text`)
//!   filters over a 1 000-thought chain.
//! - **`traversal`**: forward and backward append-order traversal at chunk sizes
//!   of 10 and 100 over a 500-thought chain, plus a filtered no-match case
//!   that exposes full-scan traversal behavior.
//! - **`import_memory_markdown`**: cost of parsing MEMORY.md format plus 100 chain
//!   appends via `import_from_memory_markdown`.
//! - **`concurrent_append_query`**: N writer threads appending and N reader threads
//!   querying simultaneously over a shared `Arc<Mutex<MentisDb>>`, exercising the
//!   state-mutex contention point at N = 2 / 4 / 8.

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use mentisdb::{
    BinaryStorageAdapter, MentisDb, ThoughtInput, ThoughtQuery, ThoughtTraversalAnchor,
    ThoughtTraversalDirection, ThoughtTraversalRequest, ThoughtType,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a fresh, empty [`MentisDb`] backed by a binary adapter in an
/// isolated temporary directory.
///
/// Returns both the chain and the [`TempDir`] guard; the guard must be kept
/// alive for the duration of the benchmark to avoid premature deletion.
fn temp_chain(label: &str) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("mentisdb-bench-{label}-"))
        .tempdir()
        .expect("failed to create tempdir for benchmark");
    let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), label);
    let mut chain =
        MentisDb::open_with_storage(Box::new(adapter)).expect("failed to open chain for benchmark");
    chain
        .set_auto_flush(true)
        .expect("failed to prime strict writer for benchmark");
    (chain, dir)
}

/// Append `count` thoughts to `chain`, cycling through three [`ThoughtType`]s
/// and tagging every thought with `"bench-tag"` plus a `"benchmark"` keyword
/// in the content.
///
/// This pre-seeds chains used by query / traversal benchmarks so those
/// benchmarks measure only retrieval, not population cost.
fn populate_chain(chain: &mut MentisDb, count: usize) {
    let types = [
        ThoughtType::Decision,
        ThoughtType::Insight,
        ThoughtType::Summary,
    ];
    for i in 0..count {
        let thought_type = types[i % types.len()];
        let input = ThoughtInput::new(thought_type, format!("benchmark thought {i}"))
            .with_tags(["bench-tag"])
            .with_importance(0.5);
        chain
            .append_thought("bench-agent", input)
            .expect("populate_chain: append failed");
    }
}

// ---------------------------------------------------------------------------
// Group 1 – append_throughput
// ---------------------------------------------------------------------------

/// Benchmark single-thought append latency on a fresh chain.
///
/// Measures the end-to-end cost per call: hash chaining, index maintenance,
/// and binary persistence to a temporary file.
pub fn bench_append_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_throughput");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    group.bench_function("append_single", |b| {
        b.iter_batched(
            || temp_chain("append-single"),
            |(mut chain, _dir)| {
                let input = ThoughtInput::new(ThoughtType::Insight, black_box("benchmark content"));
                chain
                    .append_thought(black_box("bench-agent"), input)
                    .expect("append_single: append failed");
                black_box(chain.thoughts().len());
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark sequential append of N thoughts in a single iteration.
///
/// Throughput is reported in elements per second for batch sizes of 10, 100,
/// and 1 000.  Each iteration starts from a fresh empty chain so adapter
/// initialisation cost is excluded from the measurement window.
pub fn bench_append_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_throughput");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    for n in [10u64, 100, 1_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::new("append_batch", n), &n, |b, &n| {
            b.iter_batched(
                || temp_chain("append-batch"),
                |(mut chain, _dir)| {
                    for i in 0..n {
                        let input = ThoughtInput::new(ThoughtType::Insight, format!("thought {i}"));
                        chain
                            .append_thought("bench-agent", input)
                            .expect("append_batch: append failed");
                    }
                    black_box(chain.thoughts().len());
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 2 – append_latency_percentiles
// ---------------------------------------------------------------------------

/// Measure p50 / p95 / p99 tail latency for a single-thought append.
///
/// Criterion's default output (mean ± stddev) hides tail behaviour that matters
/// most for a write-path system.  This benchmark uses `iter_custom` so every
/// individual sample duration is collected, sorted, and the percentiles are
/// printed as a side-channel via `eprintln!`.
///
/// `sample_size(10)` keeps the outer Criterion loop cheap; the real statistics
/// come from the per-sample latency vector accumulated inside `iter_custom`.
pub fn bench_append_latency_percentiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_latency_percentiles");
    group.sample_size(10); // 10 outer Criterion iterations

    group.bench_function("p50_p95_p99_single_append", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            let mut latencies = Vec::with_capacity(iters as usize);
            for _ in 0..iters {
                let (mut chain, _dir) = temp_chain("lat-pct");
                let input = ThoughtInput::new(ThoughtType::Insight, "latency probe");
                let t0 = std::time::Instant::now();
                chain.append_thought("bench-agent", input).unwrap();
                let elapsed = t0.elapsed();
                latencies.push(elapsed);
                total += elapsed;
            }
            latencies.sort_unstable();
            let n = latencies.len();
            if n >= 10 {
                let p50 = latencies[n / 2];
                let p95 = latencies[(n as f64 * 0.95) as usize];
                let p99 = latencies[(n as f64 * 0.99) as usize];
                eprintln!(
                    "\n[append_single latency] p50={:.3}ms  p95={:.3}ms  p99={:.3}ms  n={}",
                    p50.as_secs_f64() * 1000.0,
                    p95.as_secs_f64() * 1000.0,
                    p99.as_secs_f64() * 1000.0,
                    n
                );
            }
            total
        });
    });

    group.finish();
}

/// Measure p50 / p95 / p99 tail latency for a batch-of-100 append.
///
/// Each sample times one complete batch of 100 sequential appends to a fresh
/// chain.  Percentiles are computed across all Criterion samples and printed
/// via `eprintln!` for visibility in `cargo bench` output.
pub fn bench_append_batch_latency_percentiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_latency_percentiles");
    group.sample_size(10); // 10 outer Criterion iterations

    group.bench_function("p50_p95_p99_batch_100_append", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            let mut latencies = Vec::with_capacity(iters as usize);
            for _ in 0..iters {
                let (mut chain, _dir) = temp_chain("lat-pct-batch");
                let t0 = std::time::Instant::now();
                for i in 0..100u64 {
                    let input =
                        ThoughtInput::new(ThoughtType::Insight, format!("batch latency probe {i}"));
                    chain.append_thought("bench-agent", input).unwrap();
                }
                let elapsed = t0.elapsed();
                latencies.push(elapsed);
                total += elapsed;
            }
            latencies.sort_unstable();
            let n = latencies.len();
            if n >= 10 {
                let p50 = latencies[n / 2];
                let p95 = latencies[(n as f64 * 0.95) as usize];
                let p99 = latencies[(n as f64 * 0.99) as usize];
                eprintln!(
                    "\n[append_batch(100) latency] p50={:.3}ms  p95={:.3}ms  p99={:.3}ms  n={}",
                    p50.as_secs_f64() * 1000.0,
                    p95.as_secs_f64() * 1000.0,
                    p99.as_secs_f64() * 1000.0,
                    n
                );
            }
            total
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 3 – query_latency
// ---------------------------------------------------------------------------

/// Benchmark query filters over a pre-populated 1 000-thought chain.
///
/// Four sub-benchmarks exercise indexed and linear-scan retrieval paths:
/// - `query_by_type` hits the type index fast path.
/// - `query_by_text` exercises the linear content-scan path.
/// - `query_by_tag` exercises the tag posting-list fast path.
/// - `query_by_tag_miss` is a no-match tag lookup to expose miss-path overhead.
///
/// The 1 000-thought chain is built once in setup and shared across all
/// iterations; only the query call is inside the timing window.
pub fn query_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_latency");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    // Build the shared chain once; keep `_dir` alive for the group.
    let (mut seed_chain, _dir) = temp_chain("query-latency");
    populate_chain(&mut seed_chain, 1_000);
    let chain = seed_chain;

    // Benchmark type-index query.
    group.bench_function("query_by_type", |b| {
        let q = ThoughtQuery::new().with_types(vec![ThoughtType::Decision]);
        b.iter(|| {
            let results = chain.query(black_box(&q));
            black_box(results.len());
        });
    });

    // Benchmark free-text content scan.
    group.bench_function("query_by_text", |b| {
        let q = ThoughtQuery::new().with_text("benchmark");
        b.iter(|| {
            let results = chain.query(black_box(&q));
            black_box(results.len());
        });
    });

    // Benchmark tag-index lookup.
    group.bench_function("query_by_tag", |b| {
        let q = ThoughtQuery::new().with_tags_any(["bench-tag"]);
        b.iter(|| {
            let results = chain.query(black_box(&q));
            black_box(results.len());
        });
    });

    // Benchmark no-match tag lookup.
    group.bench_function("query_by_tag_miss", |b| {
        let q = ThoughtQuery::new().with_tags_any(["bench-tag-missing"]);
        b.iter(|| {
            let results = chain.query(black_box(&q));
            black_box(results.len());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 4 – traversal
// ---------------------------------------------------------------------------

/// Benchmark append-order traversal over a pre-populated 500-thought chain.
///
/// Covers three traversal scenarios:
/// - `traverse_forward_10`: 10 thoughts forward from genesis.
/// - `traverse_forward_100`: 100 thoughts forward from genesis.
/// - `traverse_backward_10`: 10 thoughts backward from head.
/// - `traverse_filtered_miss_10`: filter-miss traversal that scans the full chain.
///
/// The 500-thought chain is built once in setup; only the traversal call is
/// inside the timing window.
pub fn traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("traversal");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    // Build the shared chain once.
    let (mut seed_chain, _dir) = temp_chain("traversal");
    populate_chain(&mut seed_chain, 500);
    let chain = seed_chain;

    // Forward from genesis, 10 thoughts.
    group.bench_function("traverse_forward_10", |b| {
        let req = ThoughtTraversalRequest::new(
            ThoughtTraversalAnchor::Genesis,
            ThoughtTraversalDirection::Forward,
            10,
        )
        .with_include_anchor(true);
        b.iter(|| {
            let page = chain
                .traverse_thoughts(black_box(&req))
                .expect("traverse_forward_10: traversal failed");
            black_box(page.thoughts.len());
        });
    });

    // Forward from genesis, 100 thoughts.
    group.bench_function("traverse_forward_100", |b| {
        let req = ThoughtTraversalRequest::new(
            ThoughtTraversalAnchor::Genesis,
            ThoughtTraversalDirection::Forward,
            100,
        )
        .with_include_anchor(true);
        b.iter(|| {
            let page = chain
                .traverse_thoughts(black_box(&req))
                .expect("traverse_forward_100: traversal failed");
            black_box(page.thoughts.len());
        });
    });

    // Backward from head, 10 thoughts.
    group.bench_function("traverse_backward_10", |b| {
        let req = ThoughtTraversalRequest::new(
            ThoughtTraversalAnchor::Head,
            ThoughtTraversalDirection::Backward,
            10,
        )
        .with_include_anchor(true);
        b.iter(|| {
            let page = chain
                .traverse_thoughts(black_box(&req))
                .expect("traverse_backward_10: traversal failed");
            black_box(page.thoughts.len());
        });
    });

    // Forward from genesis, filter that matches nothing (full-chain scan).
    group.bench_function("traverse_filtered_miss_10", |b| {
        let req = ThoughtTraversalRequest::new(
            ThoughtTraversalAnchor::Genesis,
            ThoughtTraversalDirection::Forward,
            10,
        )
        .with_include_anchor(true)
        .with_filter(ThoughtQuery::new().with_text("no-match-token"));
        b.iter(|| {
            let page = chain
                .traverse_thoughts(black_box(&req))
                .expect("traverse_filtered_miss_10: traversal failed");
            black_box(page.thoughts.len());
        });
    });

    group.finish();
}

/// Benchmark `import_from_memory_markdown` against a pre-generated markdown
/// string containing 100 thoughts.
///
/// This measures the cost of parsing the MEMORY.md format plus 100 chain
/// appends.
pub fn bench_import_memory_markdown(c: &mut Criterion) {
    let mut group = c.benchmark_group("import_memory_markdown");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    // Pre-build a 100-thought chain and export its markdown once.
    // The benchmark measures only the import, not the export.
    let build_markdown = || {
        let dir = tempfile::Builder::new()
            .prefix("mentisdb-bench-import-src-")
            .tempdir()
            .expect("failed to create tempdir for import benchmark source");
        let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), "import-src");
        let mut src =
            MentisDb::open_with_storage(Box::new(adapter)).expect("failed to open source chain");
        populate_chain(&mut src, 100);
        (src.to_memory_markdown(None), dir)
    };

    let (markdown, _src_dir) = build_markdown();

    group.throughput(criterion::Throughput::Elements(100));
    group.bench_function("import_100_thoughts", |b| {
        b.iter_batched(
            || {
                let dir = tempfile::Builder::new()
                    .prefix("mentisdb-bench-import-dst-")
                    .tempdir()
                    .expect("failed to create tempdir for import benchmark destination");
                let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), "import-dst");
                let mut chain = MentisDb::open_with_storage(Box::new(adapter))
                    .expect("failed to open destination chain");
                chain
                    .set_auto_flush(true)
                    .expect("failed to prime strict writer for import benchmark");
                (chain, dir)
            },
            |(mut chain, _dir)| {
                let indices = chain
                    .import_from_memory_markdown(black_box(&markdown), "bench-agent")
                    .expect("import_100_thoughts: import failed");
                black_box(indices.len());
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 6 – concurrent_append_query
// ---------------------------------------------------------------------------

/// Benchmark concurrent append and query throughput under mutex contention.
///
/// Models real multi-agent workloads where N writer threads append thoughts
/// while N reader threads issue queries simultaneously, all sharing a single
/// `Arc<Mutex<MentisDb>>`.
///
/// The chain is pre-populated with 500 thoughts before each iteration.  All
/// threads synchronise at a [`std::sync::Barrier`] so writes and reads begin
/// at the same instant.  Wall-time throughput is reported as total operations
/// (N_writers × 50 + N_readers × 50) / elapsed seconds.
///
/// Thread counts tested: N = 2, 4, 8.
pub fn bench_concurrent_append_query(c: &mut Criterion) {
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Instant;

    let mut group = c.benchmark_group("concurrent_append_query");
    group.measurement_time(std::time::Duration::from_secs(15));

    for n_threads in [2usize, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("writers_and_readers", n_threads),
            &n_threads,
            |b, &n| {
                b.iter_batched(
                    || {
                        let (mut chain, dir) = temp_chain("concurrent");
                        populate_chain(&mut chain, 500);
                        (Arc::new(Mutex::new(chain)), dir)
                    },
                    |(chain, _dir)| {
                        let barrier = Arc::new(Barrier::new(n * 2));
                        let mut handles = Vec::with_capacity(n * 2);
                        let start = Arc::new(std::sync::Mutex::new(None::<Instant>));

                        // Writer threads
                        for i in 0..n {
                            let chain = Arc::clone(&chain);
                            let barrier = Arc::clone(&barrier);
                            handles.push(std::thread::spawn(move || {
                                barrier.wait();
                                for j in 0..50 {
                                    let input = ThoughtInput::new(
                                        ThoughtType::Insight,
                                        format!("concurrent write {i}-{j}"),
                                    );
                                    chain
                                        .lock()
                                        .unwrap()
                                        .append_thought("writer", input)
                                        .unwrap();
                                }
                            }));
                        }

                        // Reader threads
                        for _ in 0..n {
                            let chain = Arc::clone(&chain);
                            let barrier = Arc::clone(&barrier);
                            let q = ThoughtQuery::new().with_types(vec![ThoughtType::Decision]);
                            handles.push(std::thread::spawn(move || {
                                barrier.wait();
                                for _ in 0..50 {
                                    black_box(chain.lock().unwrap().query(&q).len());
                                }
                            }));
                        }

                        // Record wall-time start just after all threads are
                        // spawned (the barrier ensures they all begin together).
                        *start.lock().unwrap() = Some(Instant::now());

                        for h in handles {
                            h.join().unwrap();
                        }

                        let elapsed = start.lock().unwrap().unwrap().elapsed();
                        let total_ops = (n * 50 + n * 50) as f64;
                        let throughput = total_ops / elapsed.as_secs_f64();
                        // Expose throughput to Criterion's black_box to prevent
                        // the compiler from eliminating the measurement.
                        black_box(throughput);
                        black_box(chain.lock().unwrap().thoughts().len())
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion wiring
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_append_single,
    bench_append_batch,
    bench_append_latency_percentiles,
    bench_append_batch_latency_percentiles,
    query_latency,
    traversal,
    bench_import_memory_markdown,
    bench_concurrent_append_query
);
criterion_main!(benches);
