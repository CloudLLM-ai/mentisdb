use mentisdb::search::{EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector};
use mentisdb::{
    MentisDb, RankedSearchBackend, RankedSearchQuery, ThoughtQuery, ThoughtType, VectorSearchQuery,
};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use tempfile::TempDir;

#[derive(Debug, Clone)]
struct TestProviderError(String);

impl fmt::Display for TestProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for TestProviderError {}

#[derive(Clone)]
struct TestSemanticProvider {
    metadata: EmbeddingMetadata,
}

impl TestSemanticProvider {
    fn new(model_id: &str, embedding_version: &str) -> Self {
        Self {
            metadata: EmbeddingMetadata::new(model_id, 2, embedding_version),
        }
    }

    fn vector_for_text(&self, text: &str) -> Vec<f32> {
        let normalized = text.to_ascii_lowercase();
        if normalized.contains("latency")
            || normalized.contains("performance")
            || normalized.contains("budget")
        {
            vec![1.0, 0.0]
        } else if normalized.contains("invoice")
            || normalized.contains("vendor")
            || normalized.contains("payment")
        {
            vec![0.0, 1.0]
        } else {
            vec![0.2, 0.2]
        }
    }
}

impl EmbeddingProvider for TestSemanticProvider {
    type Error = TestProviderError;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, Self::Error> {
        Ok(inputs
            .iter()
            .map(|input| EmbeddingVector::new(self.vector_for_text(&input.text)))
            .collect())
    }
}

fn build_chain() -> (TempDir, MentisDb) {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let mut chain = MentisDb::open_with_key(&chain_dir, "semantic-search").unwrap();
    chain
        .append(
            "planner",
            ThoughtType::Decision,
            "Latency budget for the Europe rollout.",
        )
        .unwrap();
    chain
        .append(
            "accounting",
            ThoughtType::Insight,
            "Invoice reconciliation for vendor payments.",
        )
        .unwrap();
    (tempdir, chain)
}

#[test]
fn rebuild_and_query_vector_sidecar_returns_semantic_hits() {
    let (_tempdir, chain) = build_chain();
    let provider = TestSemanticProvider::new("local-test", "v1");

    let sidecar = chain.rebuild_vector_sidecar(&provider).unwrap();
    let path = chain.vector_sidecar_path(provider.metadata()).unwrap();
    assert!(path.exists());
    assert_eq!(sidecar.metadata.model_id, "local-test");

    let result = chain
        .query_vector(
            &provider,
            &VectorSearchQuery::new("performance budget").with_limit(2),
        )
        .unwrap();

    assert_eq!(result.metadata.embedding_version, "v1");
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.hits.len(), 2);
    assert_eq!(
        result.hits[0].thought.content,
        "Latency budget for the Europe rollout."
    );
}

#[test]
fn vector_sidecar_status_turns_stale_after_append() {
    let (_tempdir, mut chain) = build_chain();
    let provider = TestSemanticProvider::new("local-test", "v1");
    let sidecar = chain.rebuild_vector_sidecar(&provider).unwrap();

    chain
        .append(
            "planner",
            ThoughtType::Idea,
            "Tail-latency mitigation for the next release.",
        )
        .unwrap();

    let result = chain
        .query_vector(
            &provider,
            &VectorSearchQuery::new("performance budget").with_limit(2),
        )
        .unwrap();

    assert!(matches!(
        result.freshness,
        mentisdb::search::VectorSidecarFreshness::StaleThoughtCount { .. }
            | mentisdb::search::VectorSidecarFreshness::StaleHeadHash { .. }
    ));
    assert_eq!(sidecar.entries.len(), 2);
    assert!(!result.hits.is_empty());
}

#[test]
fn managed_vector_sidecar_stays_fresh_after_append() {
    let (_tempdir, mut chain) = build_chain();
    let provider = TestSemanticProvider::new("local-test", "v1");

    let sidecar = chain.manage_vector_sidecar(provider.clone()).unwrap();
    assert_eq!(sidecar.entries.len(), 2);
    assert_eq!(
        chain.managed_vector_sidecars(),
        vec![provider.metadata().clone()]
    );

    chain
        .append(
            "planner",
            ThoughtType::Idea,
            "Tail-latency mitigation for the next release.",
        )
        .unwrap();

    let result = chain
        .query_vector(
            &provider,
            &VectorSearchQuery::new("performance budget").with_limit(3),
        )
        .unwrap();

    assert_eq!(
        result.freshness,
        mentisdb::search::VectorSidecarFreshness::Fresh
    );
    assert_eq!(result.total_candidates, 3);
    assert_eq!(result.hits.len(), 3);
    let hit_contents: Vec<_> = result
        .hits
        .iter()
        .map(|hit| hit.thought.content.as_str())
        .collect();
    assert!(hit_contents.contains(&"Latency budget for the Europe rollout."));
    assert!(hit_contents.contains(&"Tail-latency mitigation for the next release."));

    let sidecar = chain
        .load_vector_sidecar(provider.metadata())
        .unwrap()
        .unwrap();
    assert_eq!(sidecar.entries.len(), 3);
    assert!(chain.unmanage_vector_sidecar(provider.metadata()));
    assert!(chain.managed_vector_sidecars().is_empty());
}

#[test]
fn ranked_search_blends_managed_vector_sidecars_for_semantic_only_hits() {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let mut chain = MentisDb::open_with_key(&chain_dir, "semantic-ranked").unwrap();
    chain
        .append(
            "planner",
            ThoughtType::Decision,
            "Tail latency ceiling for the Europe rollout.",
        )
        .unwrap();
    chain
        .append(
            "accounting",
            ThoughtType::Insight,
            "Invoice reconciliation for vendor payments.",
        )
        .unwrap();

    let provider = TestSemanticProvider::new("local-test", "v1");
    chain.manage_vector_sidecar(provider).unwrap();

    let ranked = chain.query_ranked(&RankedSearchQuery::new().with_text("performance budget"));

    assert_eq!(ranked.backend, RankedSearchBackend::Hybrid);
    assert_eq!(ranked.total_candidates, 1);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Tail latency ceiling for the Europe rollout."
    );
    assert_eq!(ranked.hits[0].score.lexical, 0.0);
    assert!(ranked.hits[0].score.vector > 0.0);
}

#[test]
fn vector_sidecar_paths_separate_model_versions() {
    let (_tempdir, chain) = build_chain();
    let provider_v1 = TestSemanticProvider::new("local-test", "v1");
    let provider_v2 = TestSemanticProvider::new("local-test", "v2");

    chain.rebuild_vector_sidecar(&provider_v1).unwrap();
    chain.rebuild_vector_sidecar(&provider_v2).unwrap();

    let path_v1 = chain.vector_sidecar_path(provider_v1.metadata()).unwrap();
    let path_v2 = chain.vector_sidecar_path(provider_v2.metadata()).unwrap();
    assert!(path_v1.exists());
    assert!(path_v2.exists());
    assert_ne!(path_v1, path_v2);
}

#[test]
fn corruption_does_not_break_plain_chain_queries() {
    let (_tempdir, chain) = build_chain();
    let provider = TestSemanticProvider::new("local-test", "v1");
    chain.rebuild_vector_sidecar(&provider).unwrap();
    let sidecar_path = chain.vector_sidecar_path(provider.metadata()).unwrap();
    let corrupted = std::fs::read_to_string(&sidecar_path)
        .unwrap()
        .replace("\"digest_hex\":\"", "\"digest_hex\":\"corrupted-");
    std::fs::write(&sidecar_path, corrupted).unwrap();

    let error = chain
        .query_vector(
            &provider,
            &VectorSearchQuery::new("performance budget").with_limit(2),
        )
        .unwrap_err();
    assert!(error.to_string().contains("integrity"));

    let plain = chain.query(&ThoughtQuery::new().with_text("invoice"));
    assert_eq!(plain.len(), 1);
    assert_eq!(
        plain[0].content,
        "Invoice reconciliation for vendor payments."
    );
}
