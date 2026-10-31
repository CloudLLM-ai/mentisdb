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

/// Whether the command line filter (if any) selects benchmarks whose name
/// contains `group`. Fixtures are built eagerly, so a filtered run must skip
/// the expensive ones it will not measure.
fn selected(group: &str) -> bool {
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    filters.is_empty()
        || filters
            .iter()
            .any(|f| group.contains(f.as_str()) || f.contains(group))
}

fn bench_lexical_scan(c: &mut Criterion) {
    if !selected("lexical_scan") {
        return;
    }
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
    if !selected("append_dedup") {
        return;
    }
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
    if !selected("append_sidecar") {
        return;
    }
    let mut group = c.benchmark_group("append_sidecar");
    group.sample_size(30);
    for (label, managed, base) in [
        ("no_sidecar", false, 200),
        ("local_sidecar", true, 200),
        ("local_sidecar_3000_base", true, 3_000),
    ] {
        let (mut chain, _dir) = temp_chain(label);
        if managed {
            chain
                .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
                .expect("manage sidecar");
        }
        populate(&mut chain, base);
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

/// One snapshot compaction of a managed vector sidecar: a full-corpus JSON
/// serialization plus SHA-256 plus the snapshot write. The append path pays it
/// every `VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD` appends.
fn bench_sidecar_compaction(c: &mut Criterion) {
    use mentisdb::search::{EmbeddingMetadata, VectorSidecar, VectorSidecarEntry};
    if !selected("sidecar_compaction") {
        return;
    }
    let mut group = c.benchmark_group("sidecar_compaction");
    group.sample_size(10);
    for count in [2_000_usize, 10_000] {
        let metadata = EmbeddingMetadata::new("bench", 384, "v1");
        let entries: Vec<VectorSidecarEntry> = (0..count)
            .map(|i| {
                let vector = (0..384)
                    .map(|d| ((i * 31 + d * 17) % 1000) as f32 / 997.0 - 0.5)
                    .collect();
                VectorSidecarEntry::new(
                    uuid::Uuid::from_u128(i as u128 + 1),
                    i as u64,
                    format!("h{i}"),
                    vector,
                )
            })
            .collect();
        let mut sidecar = VectorSidecar::build(
            "bench",
            metadata,
            count,
            Some(format!("h{}", count - 1)),
            chrono::Utc::now(),
            entries,
        )
        .expect("sidecar");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sidecar.json");
        group.bench_function(format!("compact_{count}x384"), |b| {
            b.iter(|| sidecar.compact_to_path(black_box(&path)).expect("compact"))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_lexical_scan,
    bench_append_dedup,
    bench_dedup_tokenization,
    bench_append_sidecar,
    bench_sidecar_compaction
);
criterion_main!(benches);
