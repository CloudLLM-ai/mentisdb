//! `ThoughtQuery::limit` must not truncate the candidate set before ranking.

use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::{MentisDb, RankedSearchQuery, ThoughtInput, ThoughtQuery, ThoughtType};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn open_chain(label: &str) -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_filter_limit_{label}_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    MentisDb::open_with_key(&dir, label).unwrap()
}

/// The single best lexical match is the oldest thought. A `filter.limit` of 5
/// (which `query` interprets as "most recent 5") must not hide it from ranking.
#[test]
fn ranked_search_ignores_filter_limit_when_selecting_candidates() {
    let mut chain = open_chain("limit");
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Idea, "zebra quokka marmot"),
        )
        .unwrap();
    for i in 0..29 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::Idea, format!("filler thought number {i}")),
            )
            .unwrap();
    }

    let request = RankedSearchQuery::new()
        .with_filter(ThoughtQuery::new().with_limit(5))
        .with_text("zebra quokka marmot")
        .with_limit(3);
    let result = chain.query_ranked(&request);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].thought.index, 0);
    assert_eq!(result.total_candidates, 1);
}

/// Without text, ranked search over all candidates still honors the ranked limit.
#[test]
fn ranked_limit_still_applies_to_returned_hits() {
    let mut chain = open_chain("limit-heuristic");
    for i in 0..12 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::Idea, format!("note {i}")),
            )
            .unwrap();
    }
    let request = RankedSearchQuery::new()
        .with_filter(ThoughtQuery::new().with_limit(2))
        .with_limit(7);
    let result = chain.query_ranked(&request);
    assert_eq!(result.hits.len(), 7);
    assert_eq!(result.total_candidates, 12);
}
