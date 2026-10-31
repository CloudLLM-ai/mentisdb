//! Regression tests for the lexical document-frequency gate and [`DfScope`].
//!
//! Found by `examples/recall_audit.rs` on real agent chains: words that occur
//! in an agent's registry text (project names, "developer", "owner") were
//! gated out of `content` because the gate compared one chain-wide document
//! frequency, which counts every thought of that agent as containing them.

use mentisdb::search::lexical::{Bm25DfCutoffs, DfScope, LexicalIndex, LexicalQuery};
use mentisdb::{MentisDb, RankedSearchQuery, ThoughtInput, ThoughtType};
use tempfile::tempdir;

/// 30 thoughts by one agent whose description mentions "gadget"; only two
/// thoughts mention a gadget in their content.
fn chain_with_registry_inflated_term() -> MentisDb {
    let dir = tempdir().unwrap();
    let mut db = MentisDb::open_with_key(dir.path(), "df-scope-tests").unwrap();
    db.upsert_agent(
        "builder",
        Some("Builder"),
        None,
        Some("Maintains the gadget project"),
        None,
    )
    .unwrap();
    for i in 0..30 {
        let content = if i == 3 || i == 17 {
            format!("note {i}: the gadget calibration drifted after the firmware update")
        } else {
            format!("note {i}: routine cleanup of unrelated subsystem number {i}")
        };
        db.append_thought("builder", ThoughtInput::new(ThoughtType::Finding, content))
            .unwrap();
    }
    // Keep the temp dir alive for the lifetime of the chain.
    std::mem::forget(dir);
    db
}

/// Under the shipped `Global` scope a word in the agent's registry text counts
/// as present in all 30 thoughts, so it is gated out of every field and the
/// two thoughts that really talk about gadgets are unreachable through it.
#[test]
fn test_global_scope_gates_term_inflated_by_registry_text() {
    let db = chain_with_registry_inflated_term();
    let index = LexicalIndex::build_with_registry(db.thoughts(), db.agent_registry());
    assert_eq!(index.document_frequency("gadget"), 30);
    let hits = index.search(&LexicalQuery::new("gadget"));
    assert!(
        hits.is_empty(),
        "global DF of 100% gates the term: {hits:?}"
    );
}

/// Under `PerField` the content field is judged on its own: "gadget" is in
/// 2/30 contents, so exactly those two thoughts match, and the registry field
/// (100% of thoughts, above its 60% cutoff) stays gated.
#[test]
fn test_per_field_scope_keeps_content_term_inflated_by_registry_text() {
    let db = chain_with_registry_inflated_term();
    let index = LexicalIndex::build_with_registry(db.thoughts(), db.agent_registry());
    let hits = index.search(&LexicalQuery::new("gadget").with_df_scope(DfScope::PerField));
    let mut found: Vec<u64> = hits.iter().map(|h| h.thought_index).collect();
    found.sort_unstable();
    assert_eq!(found, vec![3, 17]);
}

/// The scope reaches the ranked-search path through `RankedSearchQuery`.
#[test]
fn test_ranked_query_df_scope_changes_hits() {
    let db = chain_with_registry_inflated_term();
    let global = db.query_ranked(&RankedSearchQuery::new().with_text("gadget"));
    assert!(global.hits.is_empty());
    let per_field = db.query_ranked(
        &RankedSearchQuery::new()
            .with_text("gadget")
            .with_df_scope(DfScope::PerField),
    );
    let mut found: Vec<u64> = per_field.hits.iter().map(|h| h.thought.index).collect();
    found.sort_unstable();
    assert_eq!(found, vec![3, 17]);
}

/// Raising the content cutoff above the term's ratio is the other way to keep
/// such a term; it must also flow through `RankedSearchQuery`.
#[test]
fn test_ranked_query_df_cutoff_override_disables_gate() {
    let db = chain_with_registry_inflated_term();
    let query = RankedSearchQuery::new()
        .with_text("gadget")
        .with_df_cutoffs(Bm25DfCutoffs {
            content: 1.0,
            tags: 1.0,
            concepts: 1.0,
            agent_id: 1.0,
            agent_registry: 1.0,
        });
    assert!(db.query_ranked(&query).hits.len() >= 2);
}

/// Chains below 20 thoughts are never gated, in either scope.
#[test]
fn test_small_chain_is_never_gated() {
    let dir = tempdir().unwrap();
    let mut db = MentisDb::open_with_key(dir.path(), "df-scope-small").unwrap();
    for i in 0..5 {
        db.append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Finding, format!("common word {i}")),
        )
        .unwrap();
    }
    let index = LexicalIndex::build_with_registry(db.thoughts(), db.agent_registry());
    for scope in [DfScope::Global, DfScope::PerField] {
        let hits = index.search(&LexicalQuery::new("common").with_df_scope(scope));
        assert_eq!(hits.len(), 5, "{scope}");
    }
}

/// `DfScope` round-trips through its string form and rejects junk.
#[test]
fn test_df_scope_display_fromstr_roundtrip() {
    for scope in [DfScope::Global, DfScope::PerField] {
        assert_eq!(scope.to_string().parse::<DfScope>().unwrap(), scope);
    }
    assert!("sideways".parse::<DfScope>().is_err());
}
