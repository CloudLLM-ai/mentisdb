use mentisdb::search::lexical::{
    normalize_lexical_tokens, LexicalField, LexicalIndex, LexicalMatchSource, LexicalQuery,
    LEXICAL_INDEX_FORMAT_VERSION, LEXICAL_NORMALIZER_VERSION,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};
use tempfile::tempdir;

fn build_test_chain() -> MentisDb {
    let temp = tempdir().unwrap();
    let chain_dir = temp.path().to_path_buf();
    let mut db = MentisDb::open_with_key(&chain_dir, "search-lexical-tests").unwrap();

    db.append_thought(
        "dirac",
        ThoughtInput::new(
            ThoughtType::Decision,
            "Use BM25 ranking for lexical retrieval and document scoring.",
        )
        .with_importance(0.95)
        .with_tags(["ranking", "search"])
        .with_concepts(["bm25", "retrieval"]),
    )
    .unwrap();

    db.append_thought(
        "dirac",
        ThoughtInput::new(
            ThoughtType::Insight,
            "Graph expansion should happen after the lexical seed set is ranked.",
        )
        .with_importance(0.7)
        .with_tags(["graph", "expansion"])
        .with_concepts(["reasoning"]),
    )
    .unwrap();

    db.append_thought(
        "dirac",
        ThoughtInput::new(
            ThoughtType::Constraint,
            "The lexical index is derived state and must be safe to rebuild.",
        )
        .with_importance(0.9)
        .with_tags(["integrity"])
        .with_concepts(["derived-state", "bm25"]),
    )
    .unwrap();

    db
}

#[test]
fn normalizer_lowercases_and_splits_on_non_alphanumeric_boundaries() {
    let tokens = normalize_lexical_tokens("BM25-style Search_v1; Graph+Expansion");
    assert_eq!(
        tokens,
        vec!["bm25", "style", "search", "v1", "graph", "expansion"]
    );
}

#[test]
fn lexical_index_metadata_tracks_versions_and_head_hash() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    assert_eq!(
        index.metadata().index_format_version,
        LEXICAL_INDEX_FORMAT_VERSION
    );
    assert_eq!(
        index.metadata().normalizer_version,
        LEXICAL_NORMALIZER_VERSION
    );
    assert_eq!(index.metadata().thought_count, 3);
    assert_eq!(
        index.metadata().head_hash,
        db.head_hash().map(str::to_string)
    );
    assert!(index.metadata().is_current_format());
    assert!(index.metadata().matches_thoughts(db.thoughts()));
}

#[test]
fn lexical_index_builds_postings_and_document_stats() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    assert_eq!(index.document_count(), 3);
    assert!(index.term_count() >= 10);
    let postings = index.postings("bm25").unwrap();
    assert_eq!(postings.len(), 2);
    assert_eq!(postings[0].doc_position, 0);
    assert_eq!(postings[0].content_term_frequency, 1);
    assert_eq!(postings[0].concept_term_frequency, 1);
    assert_eq!(postings[1].doc_position, 2);

    let stats = index.document_stats();
    assert_eq!(stats.len(), 3);
    assert_eq!(stats[0].doc_position, 0);
    assert!(stats[0].content_len > 0);
    assert_eq!(
        stats[0].field_len(LexicalField::Content),
        stats[0].content_len
    );
    assert!((index.average_field_length(LexicalField::Content) - (32.0 / 3.0)).abs() < 0.0001);
}

#[test]
fn lexical_search_ranks_strong_content_match_before_weaker_concept_only_match() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    let hits = index.search(&LexicalQuery::new("bm25 ranking retrieval"));

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].thought_index, 0);
    assert_eq!(hits[1].thought_index, 2);
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn lexical_metadata_detects_stale_snapshot_after_new_append() {
    let temp = tempdir().unwrap();
    let chain_dir = temp.path().to_path_buf();
    let mut db = MentisDb::open_with_key(&chain_dir, "search-lexical-stale").unwrap();
    db.append_thought(
        "dirac",
        ThoughtInput::new(ThoughtType::Decision, "Initial lexical snapshot"),
    )
    .unwrap();

    let metadata = LexicalIndex::build(db.thoughts()).metadata().clone();
    assert!(metadata.matches_thoughts(db.thoughts()));

    db.append_thought(
        "dirac",
        ThoughtInput::new(
            ThoughtType::Summary,
            "A later append should stale the metadata",
        ),
    )
    .unwrap();

    assert!(!metadata.matches_thoughts(db.thoughts()));
}

#[test]
fn repeated_query_terms_do_not_double_count_scores() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    let single = index.search(&LexicalQuery::new("bm25"));
    let repeated = index.search(&LexicalQuery::new("bm25 bm25 bm25"));

    assert_eq!(single, repeated);
}

#[test]
fn lexical_search_can_rank_within_candidate_positions_only() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    let hits = index.search_in_positions(&LexicalQuery::new("bm25"), &[2]);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].thought_index, 2);
}

#[test]
fn lexical_search_returns_no_hits_for_empty_query_text() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    assert!(index.search(&LexicalQuery::new("   ---   ")).is_empty());
}

#[test]
fn lexical_search_indexes_agent_id_and_registry_text() {
    let temp = tempdir().unwrap();
    let chain_dir = temp.path().to_path_buf();
    let mut db = MentisDb::open_with_key(&chain_dir, "search-lexical-agent-registry").unwrap();
    db.upsert_agent(
        "rust-backend-engineer",
        Some("Rust Backend Engineer"),
        Some("mentisdb"),
        Some("BM25 and lexical retrieval specialist"),
        None,
    )
    .unwrap();
    db.append_thought(
        "rust-backend-engineer",
        ThoughtInput::new(
            ThoughtType::Summary,
            "Derived indexes should stay rebuildable and append-only.",
        ),
    )
    .unwrap();

    let index = LexicalIndex::build_with_registry(db.thoughts(), db.agent_registry());

    let agent_id_hits = index.search(&LexicalQuery::new("backend engineer"));
    assert_eq!(agent_id_hits.len(), 1);
    assert_eq!(agent_id_hits[0].thought_index, 0);
    assert_eq!(agent_id_hits[0].matched_terms, vec!["backend", "engineer"]);
    assert!(agent_id_hits[0]
        .match_sources
        .contains(&LexicalMatchSource::AgentId));

    let registry_hits = index.search(&LexicalQuery::new("specialist"));
    assert_eq!(registry_hits.len(), 1);
    assert_eq!(registry_hits[0].thought_index, 0);
    assert_eq!(registry_hits[0].matched_terms, vec!["specialist"]);
    assert!(registry_hits[0]
        .match_sources
        .contains(&LexicalMatchSource::AgentRegistry));
}

#[test]
fn lexical_hits_report_all_matching_sources() {
    let db = build_test_chain();
    let index = LexicalIndex::build(db.thoughts());

    let hits = index.search(&LexicalQuery::new("bm25 search retrieval"));

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].thought_index, 0);
    assert_eq!(hits[0].matched_terms, vec!["bm25", "search", "retrieval"]);
    assert!(hits[0].match_sources.contains(&LexicalMatchSource::Content));
    assert!(hits[0].match_sources.contains(&LexicalMatchSource::Tags));
    assert!(hits[0]
        .match_sources
        .contains(&LexicalMatchSource::Concepts));
}
