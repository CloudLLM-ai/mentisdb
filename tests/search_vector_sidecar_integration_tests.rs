use mentisdb::search::{EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector};
#[cfg(feature = "local-embeddings")]
use mentisdb::search::{LocalTextEmbeddingProvider, VectorBackendKind};
use mentisdb::{
    chain_storage_filename, MentisDb, RankedSearchBackend, RankedSearchQuery, StorageAdapterKind,
    ThoughtQuery, ThoughtType, VectorSearchQuery,
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

#[test]
fn test_auto_edge_overlay_built_after_append() {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let mut chain = MentisDb::open_with_key(&chain_dir, "auto-edge-test").unwrap();

    chain
        .append(
            "planner",
            ThoughtType::Decision,
            "Performance budget for the Europe rollout.",
        )
        .unwrap();
    chain
        .append(
            "planner",
            ThoughtType::Decision,
            "Latency budget for the Asia rollout.",
        )
        .unwrap();

    let provider = TestSemanticProvider::new("local-test", "v1");
    chain.manage_vector_sidecar(provider).unwrap();

    assert!(
        chain.implicit_edge_count() > 0,
        "expected implicit edges between semantically similar thoughts"
    );
    assert_eq!(
        chain.implicit_edge_thought_coverage(),
        2,
        "expected both thoughts to have implicit edges"
    );
}

#[test]
fn test_auto_edge_overlay_persists_across_reopen() {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    {
        let mut chain = MentisDb::open_with_key(&chain_dir, "auto-edge-reopen-test").unwrap();
        chain
            .append(
                "planner",
                ThoughtType::Decision,
                "Performance budget for the Europe rollout.",
            )
            .unwrap();
        chain
            .append(
                "planner",
                ThoughtType::Decision,
                "Latency budget for the Asia rollout.",
            )
            .unwrap();

        let provider = TestSemanticProvider::new("local-test", "v1");
        chain.manage_vector_sidecar(provider).unwrap();

        assert!(chain.implicit_edge_count() > 0);
    }

    let mut chain = MentisDb::open_with_key(&chain_dir, "auto-edge-reopen-test").unwrap();
    chain.apply_persisted_managed_vector_sidecars().unwrap();
    assert!(
        chain.implicit_edge_count() > 0,
        "expected implicit edges to persist across reopen"
    );
}

#[test]
fn test_missing_auto_edge_overlay_rebuilds_across_reopen() {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let chain_key = "auto-edge-rebuild-test";
    let overlay_path = {
        let storage_file = chain_storage_filename(chain_key, StorageAdapterKind::Binary);
        let stem = storage_file.strip_suffix(".tcbin").unwrap();
        chain_dir.join(format!("{stem}.auto_edges.bin"))
    };

    {
        let mut chain = MentisDb::open_with_key(&chain_dir, chain_key).unwrap();
        chain
            .append(
                "planner",
                ThoughtType::Decision,
                "Performance budget for the Europe rollout.",
            )
            .unwrap();
        chain
            .append(
                "planner",
                ThoughtType::Decision,
                "Latency budget for the Asia rollout.",
            )
            .unwrap();

        let provider = TestSemanticProvider::new("local-test", "v1");
        chain.manage_vector_sidecar(provider).unwrap();
        assert!(overlay_path.exists());
    }

    std::fs::remove_file(&overlay_path).unwrap();
    assert!(!overlay_path.exists());

    let mut chain = MentisDb::open_with_key(&chain_dir, chain_key).unwrap();
    chain.apply_persisted_managed_vector_sidecars().unwrap();

    assert!(
        overlay_path.exists(),
        "expected missing .auto_edges.bin to be rebuilt on reopen"
    );
}

#[test]
fn test_no_overlay_without_vector_sidecar() {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let mut chain = MentisDb::open_with_key(&chain_dir, "auto-edge-none-test").unwrap();

    chain
        .append(
            "planner",
            ThoughtType::Decision,
            "Performance budget for the Europe rollout.",
        )
        .unwrap();
    chain
        .append(
            "planner",
            ThoughtType::Decision,
            "Latency budget for the Asia rollout.",
        )
        .unwrap();

    assert_eq!(chain.implicit_edge_count(), 0);
    assert_eq!(chain.implicit_edge_thought_coverage(), 0);
}

#[test]
fn small_managed_vector_sidecar_does_not_create_hnsw_graph() {
    let (_tempdir, mut chain) = build_chain();
    let provider = TestSemanticProvider::new("local-test", "v1");

    chain.manage_vector_sidecar(provider.clone()).unwrap();

    let graph_path = chain.vector_hnsw_graph_path(provider.metadata()).unwrap();
    assert!(
        !graph_path.exists(),
        "expected no HNSW graph for small corpus"
    );
}

#[test]
#[cfg(feature = "local-embeddings")]
fn managed_vector_sidecar_builds_hnsw_in_background() {
    let previous_threshold = std::env::var("MENTISDB_HNSW_THRESHOLD").ok();
    std::env::set_var("MENTISDB_HNSW_THRESHOLD", "5");

    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let mut chain = MentisDb::open_with_key(&chain_dir, "hnsw-bg").unwrap();
    let provider = LocalTextEmbeddingProvider::new();

    for index in 0..10 {
        chain
            .append(
                "agent",
                ThoughtType::Idea,
                &format!("invoice record {index}"),
            )
            .unwrap();
    }

    chain.manage_vector_sidecar(provider.clone()).unwrap();

    let status = chain
        .managed_vector_sidecar_statuses()
        .unwrap()
        .into_iter()
        .find(|s| s.provider_key == "local-text-v1")
        .unwrap();
    assert_eq!(status.backend_kind, Some(VectorBackendKind::Hnsw));
    assert!(status.backend_building);

    for _ in 0..200 {
        chain.poll_vector_backend_upgrades();
        let status = chain
            .managed_vector_sidecar_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.provider_key == "local-text-v1")
            .unwrap();
        if !status.backend_building {
            assert_eq!(status.backend_kind, Some(VectorBackendKind::Hnsw));
            if let Some(previous) = previous_threshold {
                std::env::set_var("MENTISDB_HNSW_THRESHOLD", previous);
            } else {
                std::env::remove_var("MENTISDB_HNSW_THRESHOLD");
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    if let Some(previous) = previous_threshold {
        std::env::set_var("MENTISDB_HNSW_THRESHOLD", previous);
    } else {
        std::env::remove_var("MENTISDB_HNSW_THRESHOLD");
    }
    panic!("HNSW background build did not complete");
}

#[test]
#[cfg(feature = "local-embeddings")]
#[ignore = "slow: builds a 50k HNSW graph"]
fn managed_vector_sidecar_persists_and_reloads_hnsw_graph() {
    let tempdir = TempDir::new().unwrap();
    let chain_dir = PathBuf::from(tempdir.path());
    let mut chain = MentisDb::open_with_key(&chain_dir, "hnsw-persist").unwrap();
    let provider = TestSemanticProvider::new("hnsw-test", "v1");

    // Append one more than the HNSW threshold so the backend is selected and
    // the implicit-edge overlay short-circuits to an empty overlay.
    let target_count = 50_001usize;
    for index in 0..target_count {
        chain
            .append(
                "agent",
                ThoughtType::Idea,
                &format!("invoice reconciliation record {index}"),
            )
            .unwrap();
    }

    let sidecar = chain.manage_vector_sidecar(provider.clone()).unwrap();
    assert_eq!(sidecar.entries.len(), target_count);

    let graph_path = chain.vector_hnsw_graph_path(provider.metadata()).unwrap();
    assert!(
        graph_path.exists(),
        "expected HNSW graph to be persisted after managing a large sidecar"
    );

    // Reopen the chain and manage again. The fresh sidecar and persisted graph
    // should be reused instead of rebuilding the graph from scratch.
    drop(chain);
    let mut chain = MentisDb::open_with_key(&chain_dir, "hnsw-persist").unwrap();
    let reloaded_sidecar = chain.manage_vector_sidecar(provider.clone()).unwrap();
    assert_eq!(reloaded_sidecar.entries.len(), target_count);
    assert!(
        graph_path.exists(),
        "expected HNSW graph to still exist after reopen"
    );

    // Vector search should still return results using the reloaded state.
    let result = chain
        .query_vector(
            &provider,
            &VectorSearchQuery::new("invoice payment").with_limit(5),
        )
        .unwrap();
    assert!(!result.hits.is_empty());
}
