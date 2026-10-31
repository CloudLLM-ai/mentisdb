//! Benchmarks for the storage hot paths audited in the storage track.
//!
//! - **`lexical_scan`**: one-term and three-term queries against a 100 000
//!   document lexical index (per-query allocation cost of `scan`).
//! - **`append_dedup`**: append latency with `dedup_threshold` set (the
//!   per-append Jaccard scan over the last 64 thoughts) versus without.
//! - **`append_sidecar`**: append latency with a managed local-text vector
//!   sidecar, where the embedding runs under the chain's write lock.

use criterion::{criterion_group, criterion_main, Criterion};
use mentisdb::search::lexical::{LexicalCandidates, LexicalIndex, LexicalQuery};
use mentisdb::search::LocalTextEmbeddingProvider;
use mentisdb::{BinaryStorageAdapter, MentisDb, ThoughtInput, ThoughtType};
use std::hint::black_box;
use tempfile::TempDir;

const VOCABULARY: &[&str] = &[
    "deployment",
    "rollback",
    "database",
    "migration",
    "latency",
    "cache",
    "index",
    "schema",
    "replica",
    "failover",
    "queue",
    "worker",
    "timeout",
    "retry",
    "backoff",
    "metrics",
    "tracing",
    "gateway",
    "token",
    "session",
    "storage",
    "compaction",
    "snapshot",
    "journal",
    "sidecar",
    "vector",
    "embedding",
    "lexical",
    "ranking",
    "graph",
    "memory",
    "agent",
    "planner",
    "review",
    "incident",
    "postmortem",
    "alert",
    "threshold",
    "capacity",
    "throughput",
    "shard",
    "lock",
    "channel",
    "stream",
    "buffer",
    "checksum",
    "signature",
    "network",
    "service",
    "client",
];

/// Deterministic pseudo-random sentence of `words` words for document `seed`.
fn sentence(seed: u64, words: usize) -> String {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..words)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            VOCABULARY[(state >> 33) as usize % VOCABULARY.len()]
        })
        .chain(seed.is_multiple_of(1000).then_some("zeppelin"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn temp_chain(label: &str) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("mentisdb-storage-bench-{label}-"))
        .tempdir()
        .expect("tempdir");
    let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), label);
    let chain = MentisDb::open_with_storage(Box::new(adapter)).expect("open chain");
    (chain, dir)
}

fn populate(chain: &mut MentisDb, count: usize) {
    for i in 0..count {
        chain
            .append_thought(
                "bench",
                ThoughtInput::new(ThoughtType::Finding, sentence(i as u64, 40)),
            )
            .expect("append");
    }
}

fn bench_lexical_scan(c: &mut Criterion) {
    let (mut chain, _dir) = temp_chain("lexical");
    populate(&mut chain, 100_000);
    let index = LexicalIndex::build(chain.thoughts());
    let mut group = c.benchmark_group("lexical_scan");
    group.sample_size(30);
    for (label, text) in [
        ("1term_rare_100k", "zeppelin"),
        ("1term_100k", "failover"),
        ("3term_100k", "failover rollback checksum"),
    ] {
        let query = LexicalQuery::new(text);
        group.bench_function(label, |b| {
            b.iter(|| black_box(index.scan(black_box(&query), LexicalCandidates::All).len()))
        });
    }
    group.finish();
}

fn bench_append_dedup(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_dedup");
    group.sample_size(30);
    for (label, threshold) in [("off", None), ("on_window64", Some(0.95_f32))] {
        let (mut chain, _dir) = temp_chain(label);
        chain.with_dedup_threshold(threshold);
        populate(&mut chain, 200);
        let mut n = 1_000_000_u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                n += 1;
                chain
                    .append_thought(
                        "bench",
                        ThoughtInput::new(ThoughtType::Finding, sentence(n, 40)),
                    )
                    .expect("append");
            })
        });
    }
    group.finish();
}

/// The work dedup used to repeat on every append: tokenizing the whole
/// 64-thought window. The cached window now does this once per thought.
fn bench_dedup_tokenization(c: &mut Criterion) {
    let window: Vec<String> = (0..64).map(|i| sentence(i, 40)).collect();
    c.bench_function("dedup_tokenize_window64", |b| {
        b.iter(|| {
            window
                .iter()
                .map(|text| {
                    mentisdb::search::lexical::normalize_lexical_tokens(black_box(text), true).len()
                })
                .sum::<usize>()
        })
    });
}

fn bench_append_sidecar(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_sidecar");
    group.sample_size(30);
    for (label, managed) in [("no_sidecar", false), ("local_sidecar", true)] {
        let (mut chain, _dir) = temp_chain(label);
        if managed {
            chain
                .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
                .expect("manage sidecar");
        }
        populate(&mut chain, 200);
        let mut n = 2_000_000_u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                n += 1;
                chain
                    .append_thought(
                        "bench",
                        ThoughtInput::new(ThoughtType::Finding, sentence(n, 40)),
                    )
                    .expect("append");
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_lexical_scan,
    bench_append_dedup,
    bench_dedup_tokenization,
    bench_append_sidecar
);
criterion_main!(benches);
