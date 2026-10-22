//! Regression tests for the dense lexical scan and candidate restriction.

use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::lexical::{
    LexicalCandidates, LexicalIndex, LexicalMatchSource, LexicalQuery,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn build_chain(docs: Vec<ThoughtInput>) -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_lexical_scan_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain = MentisDb::open_with_key(&dir, "lexical-scan").unwrap();
    for doc in docs {
        chain.append_thought("agent", doc).unwrap();
    }
    chain
}

fn idea(content: &str) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Idea, content)
}

fn sample_chain() -> MentisDb {
    build_chain(vec![
        idea("rollback strategy for deployments"),
        idea("rollback"),
        idea("unrelated lunch menu"),
        idea("deployments rollback strategy rollback rollback notes"),
        idea("strategy only here"),
        idea("another unrelated note"),
    ])
}

/// An empty explicit subset matches nothing (the old API treated an empty
/// slice as "search everything"); `All` and the legacy empty slice still scan
/// the full index.
#[test]
fn empty_subset_matches_nothing_but_all_matches_everything() {
    let chain = sample_chain();
    let index = LexicalIndex::build(chain.thoughts());
    let query = LexicalQuery::new("rollback");
    assert!(index
        .search_candidates(&query, LexicalCandidates::Subset(&[]))
        .is_empty());
    assert!(index
        .scan(&query, LexicalCandidates::Subset(&[]))
        .is_empty());
    let all = index.search_candidates(&query, LexicalCandidates::All);
    assert_eq!(all.len(), 3);
    assert_eq!(index.search_in_positions(&query, &[]).len(), 3);
}

/// Restricting to a subset yields exactly the full-index hits inside that
/// subset, with identical scores and relative order.
#[test]
fn subset_search_equals_filtered_full_search() {
    let chain = sample_chain();
    let index = LexicalIndex::build(chain.thoughts());
    let query = LexicalQuery::new("rollback strategy");
    let full = index.search(&query);
    let subset = [0usize, 3, 4, 5];
    let restricted = index.search_candidates(&query, LexicalCandidates::Subset(&subset));
    let expected: Vec<_> = full
        .iter()
        .filter(|hit| subset.contains(&hit.doc_position))
        .collect();
    assert_eq!(restricted.len(), expected.len());
    for (got, want) in restricted.iter().zip(expected) {
        assert_eq!(got.doc_position, want.doc_position);
        assert_eq!(got.score, want.score);
        assert_eq!(got.matched_terms, want.matched_terms);
        assert_eq!(got.match_sources, want.match_sources);
    }
    // Out-of-range positions are ignored rather than panicking.
    let weird = index.search_candidates(&query, LexicalCandidates::Subset(&[999]));
    assert!(weird.is_empty());
}

/// `limit` returns the head of the unlimited ordering.
#[test]
fn limit_returns_prefix_of_unlimited_order() {
    let chain = sample_chain();
    let index = LexicalIndex::build(chain.thoughts());
    let unlimited = index.search(&LexicalQuery::new("rollback strategy deployments"));
    for limit in 0..=unlimited.len() + 1 {
        let limited =
            index.search(&LexicalQuery::new("rollback strategy deployments").with_limit(limit));
        let want: Vec<usize> = unlimited
            .iter()
            .take(limit)
            .map(|hit| hit.doc_position)
            .collect();
        let got: Vec<usize> = limited.iter().map(|hit| hit.doc_position).collect();
        assert_eq!(got, want, "limit {limit}");
    }
}

/// Matched terms are reported in query order and match sources reflect the
/// fields that contributed, without per-pair allocations changing results.
#[test]
fn matched_terms_and_sources_are_reported() {
    let chain = build_chain(vec![
        idea("alpha beta").with_tags(["gamma"]),
        idea("unrelated words"),
        idea("beta only"),
    ]);
    let index = LexicalIndex::build(chain.thoughts());
    let hits = index.search(&LexicalQuery::new("alpha beta gamma"));
    let first = hits.iter().find(|hit| hit.doc_position == 0).unwrap();
    assert_eq!(first.matched_terms, vec!["alpha", "beta", "gamma"]);
    assert!(first.match_sources.contains(&LexicalMatchSource::Content));
    assert!(first.match_sources.contains(&LexicalMatchSource::Tags));
    let third = hits.iter().find(|hit| hit.doc_position == 2).unwrap();
    assert_eq!(third.matched_terms, vec!["beta"]);
    assert_eq!(third.match_sources, vec![LexicalMatchSource::Content]);
}

/// Term bitmasks must work for queries with more than 64 terms.
#[test]
fn queries_with_more_than_64_terms_report_late_terms() {
    let words: Vec<String> = (0..70).map(|i| format!("zzterm{i}x")).collect();
    let mut docs = vec![idea(&words.join(" "))];
    for i in 0..5 {
        docs.push(idea(&format!("filler {i}")));
    }
    let chain = build_chain(docs);
    let index = LexicalIndex::build(chain.thoughts());
    let hits = index.search(&LexicalQuery::new(words.join(" ")));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_terms.len(), 70);
    assert!(hits[0]
        .matched_terms
        .iter()
        .any(|t| t.starts_with("zzterm69")));
}

/// `top_scores` returns the best entries first with position tie-breaks.
#[test]
fn scan_top_scores_orders_by_score_then_position() {
    let chain = sample_chain();
    let index = LexicalIndex::build(chain.thoughts());
    let scan = index.scan(&LexicalQuery::new("rollback"), LexicalCandidates::All);
    assert_eq!(scan.len(), 3);
    let top = scan.top_scores(2);
    assert_eq!(top.len(), 2);
    assert!(top[0].1 >= top[1].1);
    assert!(scan.top_scores(0).is_empty());
    assert_eq!(scan.top_scores(10).len(), 3);
    for (position, score) in scan.top_scores(10) {
        assert_eq!(scan.score(position), Some(score));
    }
}
