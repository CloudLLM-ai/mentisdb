//! Benchmarks for the additive ranked-search surface.
//!
//! These measurements track the cost and behavior of the new ranked APIs
//! separately from the older append-order `ThoughtQuery` path.

use criterion::{criterion_group, criterion_main, Criterion};
use mentisdb::{
    BinaryStorageAdapter, MentisDb, RankedSearchQuery, ThoughtInput, ThoughtQuery, ThoughtType,
};
use std::hint::black_box;
use tempfile::TempDir;

fn temp_chain(label: &str) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("mentisdb-ranked-search-bench-{label}-"))
        .tempdir()
        .expect("failed to create tempdir for ranked search benchmark");
    let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), label);
    let mut chain = MentisDb::open_with_storage(Box::new(adapter))
        .expect("failed to open chain for ranked search benchmark");
    chain
        .set_auto_flush(true)
        .expect("failed to enable autoflush for benchmark");
    (chain, dir)
}

fn populate_ranked_chain(chain: &mut MentisDb, count: usize) {
    chain
        .upsert_agent(
            "search-ops",
            Some("Search Operations"),
            Some("retrieval-team"),
            Some("Owns ranked lexical retrieval diagnostics."),
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
            format!("ranked search document {i}: lexical retrieval notes with bm25 style ranking"),
        )
        .with_tags(["search", "ranked"])
        .with_concepts(["retrieval", "ranking"])
        .with_importance(((i % 10) as f32) / 10.0);

        if i % 19 == 0 {
            input = input
                .with_tags(["search", "ranked", "rare-token"])
                .with_concepts(["retrieval", "ranking", "tail-hits"]);
        }

        if i % 31 == 0 {
            input = input.with_agent_name("Search Operations");
        }

        chain
            .append_thought("search-ops", input)
            .expect("failed to append ranked search thought");
    }
}

pub fn ranked_search_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_ranked");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    let (mut seed_chain, _dir) = temp_chain("ranked-benchmark");
    populate_ranked_chain(&mut seed_chain, 5_000);
    let chain = seed_chain;

    group.bench_function("query_baseline_append_order", |b| {
        let query = ThoughtQuery::new().with_text("rare-token");
        b.iter(|| {
            let results = chain.query(black_box(&query));
            black_box(results.len());
        });
    });

    group.bench_function("query_ranked_lexical_content", |b| {
        let query = RankedSearchQuery::new()
            .with_text("rare-token bm25")
            .with_limit(25);
        b.iter(|| {
            let results = chain.query_ranked(black_box(&query));
            black_box(results.total_candidates);
            black_box(results.hits.first().map(|hit| hit.score.total));
        });
    });

    group.bench_function("query_ranked_filtered_lexical", |b| {
        let query = RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_types(vec![ThoughtType::Decision]))
            .with_text("lexical retrieval")
            .with_limit(25);
        b.iter(|| {
            let results = chain.query_ranked(black_box(&query));
            black_box(results.total_candidates);
            black_box(results.hits.first().map(|hit| hit.thought.index));
        });
    });

    group.bench_function("query_ranked_heuristic_no_text", |b| {
        let query = RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["ranked"]))
            .with_limit(25);
        b.iter(|| {
            let results = chain.query_ranked(black_box(&query));
            black_box(results.total_candidates);
            black_box(results.hits.first().map(|hit| hit.score.total));
        });
    });

    group.finish();
}

criterion_group!(search_ranked_benches, ranked_search_latency);
criterion_main!(search_ranked_benches);
