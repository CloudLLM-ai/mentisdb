//! Regression tests for federated RRF reranking across chains.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::{MentisDb, RankedSearchQuery, ThoughtInput, ThoughtType};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn open_chain(label: &str, docs: &[&str]) -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_federated_rrf_{label}_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain = MentisDb::open_with_key(&dir, label).unwrap();
    for doc in docs {
        chain
            .append_thought("agent", ThoughtInput::new(ThoughtType::Idea, *doc))
            .unwrap();
    }
    chain
}

/// Thoughts at the same per-chain index in different chains are different
/// documents and must receive their own RRF score instead of sharing one.
#[test]
fn federated_rrf_does_not_collide_on_per_chain_index() {
    let chain_a = open_chain(
        "fed-a",
        &["needle one", "unrelated alpha", "unrelated beta"],
    );
    let chain_b = open_chain(
        "fed-b",
        &[
            "needle two three four five six seven eight nine ten",
            "unrelated gamma",
            "unrelated delta",
        ],
    );
    let query = RankedSearchQuery::new()
        .with_text("needle")
        .with_reranking(10)
        .with_limit(10);
    let mut others = BTreeMap::new();
    others.insert("fed-b".to_string(), query.clone());

    let result = chain_a.query_federated("fed-a", &[("fed-b", &chain_b)], &query, &others, 10, 0);
    let a_hit = result
        .hits
        .iter()
        .find(|hit| hit.chain_key == "fed-a" && hit.thought.index == 0)
        .expect("chain a needle hit");
    let b_hit = result
        .hits
        .iter()
        .find(|hit| hit.chain_key == "fed-b" && hit.thought.index == 0)
        .expect("chain b needle hit");
    assert!(a_hit.score.rrf > 0.0 && b_hit.score.rrf > 0.0);
    assert!(
        a_hit.score.rrf > b_hit.score.rrf,
        "the shorter (better lexical) chain-a document must rank first: {} vs {}",
        a_hit.score.rrf,
        b_hit.score.rrf
    );
    let first = &result.hits[0];
    assert_eq!(first.chain_key, "fed-a");
}

/// Federated results must be identical across repeated calls.
#[test]
fn federated_ordering_is_deterministic() {
    let chain_a = open_chain("fed-det-a", &["needle", "needle", "other"]);
    let chain_b = open_chain("fed-det-b", &["needle", "needle", "other"]);
    let query = RankedSearchQuery::new()
        .with_text("needle")
        .with_reranking(10)
        .with_limit(10);
    let mut others = BTreeMap::new();
    others.insert("fed-det-b".to_string(), query.clone());
    let key = |r: &mentisdb::FederatedSearchResult<'_>| -> Vec<(String, u64)> {
        r.hits
            .iter()
            .map(|h| (h.chain_key.to_string(), h.thought.index))
            .collect()
    };
    let first = key(&chain_a.query_federated(
        "fed-det-a",
        &[("fed-det-b", &chain_b)],
        &query,
        &others,
        10,
        0,
    ));
    for _ in 0..5 {
        let again = key(&chain_a.query_federated(
            "fed-det-a",
            &[("fed-det-b", &chain_b)],
            &query,
            &others,
            10,
            0,
        ));
        assert_eq!(again, first);
    }
}
