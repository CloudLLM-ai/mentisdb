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

/// Collapsing weighs every hit of the ranked window against the survivors so
/// far, so a deep page used to cost the square of the window. The server caps
/// `offset + limit` at 10,000; this measures windows of 1,000 and 2,000 over
/// 3,000 same-sized, non-duplicate hits (a tenth of 30,000 thoughts carry the
/// query word, which is rare enough to stay out of the document-frequency gate),
/// the worst case for the size shortcut.
pub fn collapse_window_latency(c: &mut Criterion) {
    let (mut chain, _dir) = temp_chain("collapse-window-benchmark");
    for i in 0..30_000 {
        let word = if i % 10 == 0 { "quokka" } else { "wombat" };
        let content = format!(
            "{word} survey entry{i} covers habitat{} diet{} range{} season{} notes{}",
            i % 97,
            i % 89,
            i % 83,
            i % 79,
            i % 73
        );
        chain
            .append_thought("bench", ThoughtInput::new(ThoughtType::Finding, content))
            .expect("failed to append collapse benchmark thought");
    }
    let mut group = c.benchmark_group("search_ranked_collapse");
    group.sample_size(10);
    for limit in [1_000usize, 2_000] {
        group.bench_function(format!("window_{limit}"), |b| {
            let query = RankedSearchQuery::new()
                .with_text("quokka")
                .with_limit(limit)
                .with_collapse_duplicates(Some(0.9));
            b.iter(|| black_box(chain.query_ranked(black_box(&query)).hits.len()));
        });
    }
    group.finish();
}

/// Worst case for refilling a collapsed page: a fifth of the thoughts are
/// byte-identical matches, so they all collapse into one hit and the page
/// (limit 10) can only fill from the few distinct matches deeper down. Measures
/// 20,000 and 100,000 thoughts; the refill window is capped, so the cost must
/// not grow with the number of identical matches.
pub fn collapse_refill_latency(c: &mut Criterion) {
    std::env::set_var("MENTISDB_EMBEDDINGS", "off");
    let mut group = c.benchmark_group("search_ranked_collapse_refill");
    group.sample_size(10);
    for thoughts in [20_000usize, 100_000] {
        let (mut chain, _dir) = temp_chain(&format!("collapse-refill-{thoughts}"));
        for i in 0..thoughts {
            let content = match i % 20 {
                0..=3 => "quokka identical note".to_owned(),
                4 => format!(
                    "quokka survey entry{i} habitat{} diet{} range{}",
                    i % 97,
                    i % 89,
                    i % 83
                ),
                _ => format!(
                    "wombat survey entry{i} habitat{} diet{} range{}",
                    i % 97,
                    i % 89,
                    i % 83
                ),
            };
            chain
                .append_thought("bench", ThoughtInput::new(ThoughtType::Finding, content))
                .expect("failed to append refill benchmark thought");
        }
        group.bench_function(format!("limit10_{thoughts}"), |b| {
            let query = RankedSearchQuery::new()
                .with_text("quokka")
                .with_limit(10)
                .with_collapse_duplicates(Some(0.9));
            // Up to 20,000 thoughts the page fills from the distinct matches.
            // Beyond the refill window (4,096 slots here) it cannot, by design,
            // and returns only what the window held.
            let filled = chain.query_ranked(&query).hits.len();
            assert!(if thoughts <= 20_000 {
                filled == 10
            } else {
                filled >= 1
            });
            b.iter(|| black_box(chain.query_ranked(black_box(&query)).hits.len()));
        });
    }
    group.finish();
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

    // Cost of the version-lineage layer: the same query with successor
    // resolution off (raw ranking), on (default, ranks a 2x wider page), and on
    // with near-duplicate collapsing.
    group.bench_function("query_ranked_resolve_off", |b| {
        let query = RankedSearchQuery::new()
            .with_text("rare-token bm25")
            .with_limit(25)
            .with_resolve_successors(false)
            .with_collapse_duplicates(None);
        b.iter(|| black_box(chain.query_ranked(black_box(&query)).hits.len()));
    });
    group.bench_function("query_ranked_resolve_on", |b| {
        let query = RankedSearchQuery::new()
            .with_text("rare-token bm25")
            .with_limit(25)
            .with_resolve_successors(true)
            .with_collapse_duplicates(None);
        b.iter(|| black_box(chain.query_ranked(black_box(&query)).hits.len()));
    });
    group.bench_function("query_ranked_resolve_on_collapse", |b| {
        let query = RankedSearchQuery::new()
            .with_text("rare-token bm25")
            .with_limit(25)
            .with_resolve_successors(true)
            .with_collapse_duplicates(Some(0.85));
        b.iter(|| black_box(chain.query_ranked(black_box(&query)).hits.len()));
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

criterion_group!(
    search_ranked_benches,
    ranked_search_latency,
    collapse_window_latency,
    collapse_refill_latency
);
criterion_main!(search_ranked_benches);
