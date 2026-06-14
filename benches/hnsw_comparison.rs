//! Comparison benchmark for vector-search backends.
//!
//! Measures recall@10 and p95 query latency across:
//!
//! * Exact (`VectorIndex`) — linear scan cosine, the pre-HNSW baseline.
//! * HNSW (`HnswBackend`) — approximate graph over raw f32 vectors.
//! * Quantized HNSW (`QuantizedHnswBackend`) — approximate graph over 8-bit
//!   quantized vectors.
//!
//! Run with:
//!
//! ```bash
//! cargo bench --bench hnsw_comparison --features hnsw-backend
//! ```
//!
//! The benchmark uses synthetic unit vectors so it is deterministic and does
//! not depend on an embedding provider or the network.

use std::collections::HashSet;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use mentisdb::search::{
    EmbeddingMetadata, QuantizedHnswBackend, VectorDocument, VectorIndex, VectorQuery,
    VectorSearchBackend,
};

/// Dimension and corpus size for the comparison. 10k vectors is small enough
/// to run in CI but large enough that the exact scan is noticeably slower
/// than the approximate backends.
const DIM: usize = 128;
const N: usize = 10_000;
const K: usize = 10;

fn build_corpus(n: usize, offset: usize) -> Vec<VectorDocument> {
    (0..n)
        .map(|i| {
            let raw: Vec<f32> = (0..DIM)
                .map(|d| (((i + offset) * 31 + d * 17) % 1009) as f32 / 1009.0 - 0.5)
                .collect();
            let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
            let vector: Vec<f32> = raw.iter().map(|v| v / norm).collect();
            VectorDocument::new(format!("doc-{i:05}"), vector)
        })
        .collect()
}

fn build_queries(count: usize) -> Vec<Vec<f32>> {
    (0..count)
        .map(|q| {
            let raw: Vec<f32> = (0..DIM)
                .map(|d| ((q * 53 + d * 19) % 997) as f32 / 997.0 - 0.5)
                .collect();
            let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
            raw.iter().map(|v| v / norm).collect()
        })
        .collect()
}

fn bench_group(c: &mut Criterion) {
    let metadata = EmbeddingMetadata::new("bench", DIM, "v1");
    let documents = build_corpus(N, 0);
    let queries = build_queries(100);

    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let hnsw = mentisdb::search::hnsw_backend::HnswBackend::from_documents(
        metadata.clone(),
        documents.clone(),
    )
    .unwrap();
    let quantized = QuantizedHnswBackend::from_documents(metadata, documents).unwrap();

    let exact_recall = || -> f64 {
        // Recall is always 1.0 against itself; this keeps the data shape
        // uniform in the report.
        1.0
    };

    let hnsw_recall = compute_recall(&exact, &hnsw, &queries);
    let quantized_recall = compute_recall(&exact, &quantized, &queries);

    // Latency measurements: p95 over repeated queries.
    let mut group = c.benchmark_group("hnsw_comparison_latency");
    group.sample_size(50);
    group.measurement_time(std::time::Duration::from_secs(10));

    group.bench_function("exact", |b| {
        b.iter_batched(
            || queries.clone(),
            |qs| {
                for q in qs {
                    let _ = black_box(exact.search(&VectorQuery::new(q).with_limit(K)).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("hnsw", |b| {
        b.iter_batched(
            || queries.clone(),
            |qs| {
                for q in qs {
                    let _ = black_box(hnsw.search(&VectorQuery::new(q).with_limit(K)).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("quantized_hnsw", |b| {
        b.iter_batched(
            || queries.clone(),
            |qs| {
                for q in qs {
                    let _ = black_box(
                        quantized
                            .search(&VectorQuery::new(q).with_limit(K))
                            .unwrap(),
                    );
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();

    // Recall summary (not a real benchmark; just printed by Criterion as
    // custom counters would be overkill).
    println!("\n=== hnsw_comparison recall@10 (n={N}, dim={DIM}, k={K}) ===");
    println!("exact          : {:.3}", exact_recall());
    println!("hnsw           : {:.3}", hnsw_recall);
    println!("quantized_hnsw : {:.3}", quantized_recall);
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

criterion_group!(benches, bench_group);
criterion_main!(benches);
