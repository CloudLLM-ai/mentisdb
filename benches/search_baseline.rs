//! Benchmarks for the current lexical/filter-first search baseline.
//!
//! This benchmark suite intentionally measures the search semantics MentisDB
//! exposes today:
//!
//! - indexed narrowing by `thought_type`, tags, and concepts
//! - case-insensitive substring matching over content and registry metadata
//! - append-order result collection with newest-tail `limit` truncation
//!
//! These numbers are meant to serve as the baseline for future lexical/ranked
//! search work. If a later branch introduces BM25, hybrid ranking, or vector
//! retrieval, those new surfaces should be compared against this benchmark
//! rather than replacing it silently.

use criterion::{criterion_group, criterion_main, Criterion};
use mentisdb::{BinaryStorageAdapter, MentisDb, ThoughtInput, ThoughtQuery, ThoughtType};
use std::hint::black_box;
use tempfile::TempDir;

fn temp_chain(label: &str) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("mentisdb-search-bench-{label}-"))
        .tempdir()
        .expect("failed to create tempdir for search benchmark");
    let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), label);
    let mut chain = MentisDb::open_with_storage(Box::new(adapter))
        .expect("failed to open chain for search benchmark");
    chain
        .set_auto_flush(true)
        .expect("failed to enable autoflush for benchmark");
    (chain, dir)
}

fn populate_search_chain(chain: &mut MentisDb, count: usize) {
    chain
        .upsert_agent(
            "search-ops",
            Some("Search Operations"),
            Some("retrieval-team"),
            Some("Owns lexical retrieval tuning and search diagnostics."),
            None,
        )
        .expect("failed to upsert search-ops");
    chain
        .add_agent_alias("search-ops", "navigator")
        .expect("failed to add alias");

    let thought_types = [
        ThoughtType::Decision,
        ThoughtType::Insight,
        ThoughtType::Summary,
        ThoughtType::Constraint,
    ];

    for i in 0..count {
        let thought_type = thought_types[i % thought_types.len()];
        let mut input = ThoughtInput::new(
            thought_type,
            format!(
                "search baseline document {i}: latency notes for lexical retrieval benchmarking"
            ),
        )
        .with_tags(["search", "latency"])
        .with_concepts(["retrieval", "benchmarking"])
        .with_importance(0.5);

        if i % 17 == 0 {
            input = input
                .with_tags(["search", "latency", "rare-token"])
                .with_concepts(["retrieval", "benchmarking", "tail-hits"]);
        }

        if i % 29 == 0 {
            input = input.with_agent_name("Search Operations");
        }

        chain
            .append_thought("search-ops", input)
            .expect("failed to append baseline search thought");
    }
}

pub fn lexical_search_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_baseline");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    let (mut seed_chain, _dir) = temp_chain("lexical-baseline");
    populate_search_chain(&mut seed_chain, 5_000);
    let chain = seed_chain;

    group.bench_function("text_content_hit_sparse", |b| {
        let query = ThoughtQuery::new().with_text("rare-token");
        b.iter(|| {
            let results = chain.query(black_box(&query));
            black_box(results.len());
        });
    });

    group.bench_function("text_registry_hit", |b| {
        let query = ThoughtQuery::new().with_text("navigator");
        b.iter(|| {
            let results = chain.query(black_box(&query));
            black_box(results.len());
        });
    });

    group.bench_function("indexed_text_intersection", |b| {
        let query = ThoughtQuery::new()
            .with_types(vec![ThoughtType::Decision])
            .with_tags_any(["rare-token"])
            .with_text("latency");
        b.iter(|| {
            let results = chain.query(black_box(&query));
            black_box(results.len());
        });
    });

    group.bench_function("newest_tail_limit", |b| {
        let query = ThoughtQuery::new()
            .with_text("search baseline")
            .with_limit(25);
        b.iter(|| {
            let results = chain.query(black_box(&query));
            black_box(results.len());
            black_box(results.last().map(|thought| thought.index));
        });
    });

    group.finish();
}

criterion_group!(search_benches, lexical_search_latency);
criterion_main!(search_benches);
