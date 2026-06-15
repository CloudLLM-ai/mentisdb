//! Scale benchmark for the HNSW vector backend.
//!
//! Measures build time, p95 query latency, and recall@10 at 50k vectors by
//! default. Larger corpora are gated behind environment variables so the
//! default run completes in a reasonable amount of time:
//!
//! * `MENTISDB_BENCH_HNSW_100K=1` adds the 100k corpus.
//! * `MENTISDB_BENCH_HNSW_1M=1` adds the 1M corpus.
//!
//! Run with:
//!
//! ```bash
//! cargo bench --bench hnsw_scale --features hnsw-backend
//! ```
//!
//! To include the 100k and 1M corpora:
//!
//! ```bash
//! MENTISDB_BENCH_HNSW_100K=1 MENTISDB_BENCH_HNSW_1M=1 \\
//!     cargo bench --bench hnsw_scale --features hnsw-backend
//! ```

use std::collections::HashSet;
use std::time::Instant;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mentisdb::search::{
    EmbeddingMetadata, VectorDocument, VectorIndex, VectorQuery, VectorSearchBackend,
};
use rand::rngs::StdRng;
use rand::SeedableRng;
use rand_distr::{Distribution, StandardNormal};

const DIM: usize = 128;
const K: usize = 10;

fn build_random_unit_vector(rng: &mut StdRng) -> Vec<f32> {
    let normal = StandardNormal;
    let raw: Vec<f32> = (0..DIM)
        .map(|_| {
            let value: f64 = normal.sample(rng);
            value as f32
        })
        .collect();
    let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
    raw.iter().map(|v| v / norm).collect()
}

fn build_vectors(n: usize, seed: u64) -> Vec<VectorDocument> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n)
        .map(|i| {
            let vector = build_random_unit_vector(&mut rng);
            VectorDocument::new(format!("doc-{i:08}"), vector)
        })
        .collect()
}

fn build_queries(count: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..count)
        .map(|_| build_random_unit_vector(&mut rng))
        .collect()
}

fn run_scale_bench(c: &mut Criterion, size_label: &str, n: usize) -> f64 {
    println!("\n=== hnsw_scale {size_label} (n={n}, dim={DIM}, k={K}) ===");

    let documents = build_vectors(n, 0);
    let queries = build_queries(100, 1);

    let start = Instant::now();
    let hnsw = mentisdb::search::hnsw_backend::HnswBackend::from_documents(
        EmbeddingMetadata::new("bench", DIM, "v1"),
        documents.clone(),
    )
    .unwrap();
    let build_time = start.elapsed();
    println!("hnsw build time: {:.2}s", build_time.as_secs_f64());

    // Recall is computed against the exact backend for corpora up to 100k.
    // Computing exact nearest neighbors for a 1M corpus is intentionally
    // skipped because it would dominate the benchmark runtime without adding
    // useful information about the approximate backend.
    let recall = if n <= 100_000 {
        let exact =
            VectorIndex::from_documents(EmbeddingMetadata::new("bench", DIM, "v1"), documents)
                .unwrap();
        let r = compute_recall(&exact, &hnsw, &queries);
        println!("recall@10: {:.3}", r);
        r
    } else {
        println!("recall@10: skipped for 1M+ corpus");
        0.0
    };

    let mut group = c.benchmark_group(format!("hnsw_scale_{size_label}"));
    group.sample_size(20);
    group.measurement_time(std::time::Duration::from_secs(10));

    group.bench_function("hnsw_query", |b| {
        let mut index = 0;
        b.iter(|| {
            let q = queries[index % queries.len()].clone();
            let _ = black_box(hnsw.search(&VectorQuery::new(q).with_limit(K)).unwrap());
            index += 1;
        });
    });

    group.finish();

    recall
}

fn compute_recall<B: VectorSearchBackend>(
    exact: &VectorIndex,
    backend: &B,
    queries: &[Vec<f32>],
) -> f64 {
    let mut total = 0.0f64;
    for query in queries {
        let exact_ids: HashSet<String> = exact
            .search(&VectorQuery::new(query.clone()).with_limit(K))
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let backend_ids: HashSet<String> = backend
            .search(&VectorQuery::new(query.clone()).with_limit(K))
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let intersection = exact_ids.intersection(&backend_ids).count();
        total += intersection as f64 / K as f64;
    }
    total / queries.len() as f64
}

fn bench_group(c: &mut Criterion) {
    run_scale_bench(c, "n50k", 50_000);

    if std::env::var("MENTISDB_BENCH_HNSW_100K")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        run_scale_bench(c, "n100k", 100_000);
    }

    if std::env::var("MENTISDB_BENCH_HNSW_1M")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        run_scale_bench(c, "n1m", 1_000_000);
    }
}

criterion_group!(benches, bench_group);
criterion_main!(benches);
