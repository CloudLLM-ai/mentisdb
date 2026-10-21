//! Regression tests: thesaurus synonyms must expand for inflected query terms.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::lexical::{LexicalIndex, LexicalQuery};
use mentisdb::search::thesaurus;
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn build_chain(contents: &[&str]) -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_thesaurus_stem_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain = MentisDb::open_with_key(&dir, "thesaurus-stem").unwrap();
    for content in contents {
        chain
            .append_thought("a", ThoughtInput::new(ThoughtType::Idea, *content))
            .unwrap();
    }
    chain
}

/// "debugging" must expand to "diagnose" even though the thesaurus headword is
/// "debug" and the lexical normalizer stems the query term.
#[test]
fn inflected_query_term_matches_synonym_document() {
    let chain = build_chain(&[
        "We need to diagnose the failing deploy.",
        "Lunch menu for friday.",
    ]);
    let index = LexicalIndex::build(chain.thoughts());
    let synonyms = thesaurus::expand_text("debugging");
    assert!(!synonyms.is_empty());
    let query = LexicalQuery::new("debugging").with_synonyms(synonyms, 0.7);
    let hits = index.search(&query);
    assert_eq!(hits.len(), 1, "only the diagnose doc should match");
    assert_eq!(hits[0].doc_position, 0);
}

/// Without synonyms, the same query must not match.
#[test]
fn no_synonyms_no_match() {
    let chain = build_chain(&["We need to diagnose the failing deploy."]);
    let index = LexicalIndex::build(chain.thoughts());
    assert!(index.search(&LexicalQuery::new("debugging")).is_empty());
}

/// Raw lowercase synonym keys supplied by callers are normalized on the way in.
#[test]
fn raw_keyed_synonym_map_is_normalized_by_query() {
    let chain = build_chain(&["We need to diagnose the failing deploy."]);
    let index = LexicalIndex::build(chain.thoughts());
    let mut map = HashMap::new();
    map.insert("debugging".to_string(), vec!["diagnose".to_string()]);
    let query = LexicalQuery::new("debugging").with_synonyms(map, 0.7);
    assert_eq!(index.search(&query).len(), 1);
}

/// A term with a huge synonym list must not explode the expanded term set.
#[test]
fn synonyms_per_term_are_capped() {
    let mut map = HashMap::new();
    map.insert(
        "debug".to_string(),
        (0..50).map(|i| format!("synonym{i}")).collect::<Vec<_>>(),
    );
    let query = LexicalQuery::new("debug").with_synonyms(map, 0.7);
    let expanded = query.expanded_terms();
    assert!(expanded.len() <= 1 + 5, "got {} terms", expanded.len());
    assert!(expanded.len() > 1);
}

/// The inflected forms reported in the audit all gain synonym terms.
#[test]
fn inflected_forms_gain_synonym_terms() {
    for text in ["debugging", "running", "deployments"] {
        let map = thesaurus::expand_text(text);
        assert!(!map.is_empty(), "{text}: expand_text found nothing");
        let query = LexicalQuery::new(text).with_synonyms(map, 0.7);
        assert!(
            query.expanded_terms().len() > query.normalized_terms().len(),
            "{text} should gain synonym terms"
        );
    }
}
