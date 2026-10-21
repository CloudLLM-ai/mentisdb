//! Vector blending in `query_ranked` with and without candidate filters.
//!
//! Unfiltered queries skip building a backend id filter; filtered queries
//! restrict the backend by id. Both must resolve hits back to the right
//! thoughts and never leak non-candidates.

use mentisdb::search::{EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector};
use mentisdb::{MentisDb, RankedSearchQuery, ThoughtQuery, ThoughtType};
use std::error::Error;
use std::fmt;
use tempfile::TempDir;

#[derive(Debug, Clone)]
struct TestProviderError;

impl fmt::Display for TestProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("test provider error")
    }
}

impl Error for TestProviderError {}

#[derive(Clone)]
struct TestSemanticProvider {
    metadata: EmbeddingMetadata,
}

impl EmbeddingProvider for TestSemanticProvider {
    type Error = TestProviderError;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, Self::Error> {
        Ok(inputs
            .iter()
            .map(|input| {
                let text = input.text.to_ascii_lowercase();
                if text.contains("latency") || text.contains("performance") {
                    EmbeddingVector::new(vec![1.0, 0.0])
                } else if text.contains("invoice") {
                    EmbeddingVector::new(vec![0.0, 1.0])
                } else {
                    EmbeddingVector::new(vec![0.2, 0.2])
                }
            })
            .collect())
    }
}

fn build_chain() -> (TempDir, MentisDb) {
    let tempdir = TempDir::new().unwrap();
    let mut chain = MentisDb::open_with_key(tempdir.path(), "semantic-filter").unwrap();
    chain
        .append("planner", ThoughtType::Decision, "Tail latency ceiling.")
        .unwrap();
    chain
        .append("ops", ThoughtType::Decision, "Latency alerting rules.")
        .unwrap();
    chain
        .append(
            "accounting",
            ThoughtType::Insight,
            "Invoice reconciliation.",
        )
        .unwrap();
    chain
        .manage_vector_sidecar(TestSemanticProvider {
            metadata: EmbeddingMetadata::new("local-test", 2, "v1"),
        })
        .unwrap();
    (tempdir, chain)
}

/// With no filter both latency thoughts surface through the vector signal.
#[test]
fn unfiltered_semantic_hits_resolve_to_thoughts() {
    let (_dir, chain) = build_chain();
    let ranked = chain.query_ranked(&RankedSearchQuery::new().with_text("performance"));
    let mut contents: Vec<&str> = ranked
        .hits
        .iter()
        .map(|hit| hit.thought.content.as_str())
        .collect();
    contents.sort_unstable();
    assert_eq!(
        contents,
        vec!["Latency alerting rules.", "Tail latency ceiling."]
    );
    assert!(ranked.hits.iter().all(|hit| hit.score.vector > 0.0));
}

/// A filter restricts semantic hits to the allowed candidates.
#[test]
fn filtered_semantic_hits_stay_inside_candidates() {
    let (_dir, chain) = build_chain();
    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_agent_ids(["ops"]))
            .with_text("performance"),
    );
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(ranked.hits[0].thought.content, "Latency alerting rules.");
    assert!(ranked.hits[0].score.vector > 0.0);
}

/// A filter that matches nothing yields no hits at all.
#[test]
fn empty_candidate_set_yields_no_hits() {
    let (_dir, chain) = build_chain();
    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_agent_ids(["nobody"]))
            .with_text("performance"),
    );
    assert!(ranked.hits.is_empty());
    assert_eq!(ranked.total_candidates, 0);
}
