use crate::search::hnsw_backend::HnswBackend;
use crate::search::vector::{cosine_similarity, VectorDocument, VectorQuery, VectorSearchBackend};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::Path;
use uuid::Uuid;

/// Pairwise all-pairs is used only for tiny corpora so unit tests stay exact.
const PAIRWISE_BUILD_LIMIT: usize = 128;

/// Derived semantic neighborhood for one thought.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImplicitNeighbor {
    /// Target thought identifier.
    pub thought_id: Uuid,
    /// Cosine similarity score between the source and target thought embeddings.
    pub cosine_score: f32,
}

/// In-memory overlay of vector-inferred `RelatedTo` edges.
///
/// These edges are derived from the vector sidecar at a given threshold and K,
/// and supplement the explicit relations in [`crate::search::graph::ThoughtAdjacencyIndex`] during BFS.
/// The overlay is rebuildable from the sidecar with no loss of ground truth.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImplicitEdgeOverlay {
    /// Source thought id → top-K nearest neighbors above threshold.
    pub edges: HashMap<Uuid, Vec<ImplicitNeighbor>>,
    /// Threshold used when this overlay was built.
    pub threshold: f32,
    /// K used when this overlay was built.
    pub k: usize,
}

impl ImplicitEdgeOverlay {
    /// Create an empty overlay with the given threshold and K.
    pub fn new(threshold: f32, k: usize) -> Self {
        Self {
            edges: HashMap::new(),
            threshold,
            k,
        }
    }

    /// Build from scratch by comparing all N² pairs in the sidecar.
    ///
    /// Tiny corpora (`N <= 128`) stay pairwise so existing unit tests remain
    /// exact. Larger corpora build a temporary HNSW index and take per-vector
    /// top-k, which is ~N log N instead of N².
    pub fn build_from_sidecar(
        sidecar: &crate::search::VectorSidecar,
        k: usize,
        threshold: f32,
    ) -> Self {
        if sidecar.entries.len() <= PAIRWISE_BUILD_LIMIT {
            return Self::build_pairwise(sidecar, k, threshold);
        }
        match Self::build_with_hnsw(sidecar, k, threshold) {
            Ok(overlay) => overlay,
            Err(_) => Self::build_pairwise(sidecar, k, threshold),
        }
    }

    /// Build using an already-warm vector backend (Exact or HNSW).
    pub fn build_from_backend(
        sidecar: &crate::search::VectorSidecar,
        backend: &dyn VectorSearchBackend,
        k: usize,
        threshold: f32,
    ) -> Self {
        if sidecar.entries.len() <= PAIRWISE_BUILD_LIMIT {
            return Self::build_pairwise(sidecar, k, threshold);
        }
        Self::build_with_search(sidecar, k, threshold, |vector, limit| {
            backend
                .search(&VectorQuery::new(vector.to_vec()).with_limit(limit))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|hit| {
                    Uuid::parse_str(&hit.document_id)
                        .ok()
                        .map(|id| (id, hit.score))
                })
                .collect()
        })
    }

    fn build_pairwise(sidecar: &crate::search::VectorSidecar, k: usize, threshold: f32) -> Self {
        let mut overlay = Self::new(threshold, k);
        let entries = &sidecar.entries;
        for (i, entry_a) in entries.iter().enumerate() {
            let mut neighbors: Vec<ImplicitNeighbor> = entries
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter_map(|(_, entry_b)| {
                    let score = cosine_similarity(&entry_a.vector, &entry_b.vector)?;
                    if score >= threshold {
                        Some(ImplicitNeighbor {
                            thought_id: entry_b.thought_id,
                            cosine_score: score,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            neighbors.sort_by(|a, b| b.cosine_score.total_cmp(&a.cosine_score));
            neighbors.truncate(k);
            if !neighbors.is_empty() {
                overlay.edges.insert(entry_a.thought_id, neighbors);
            }
        }
        overlay
    }

    fn build_with_hnsw(
        sidecar: &crate::search::VectorSidecar,
        k: usize,
        threshold: f32,
    ) -> Result<Self, crate::search::VectorIndexError> {
        let documents: Vec<VectorDocument> = sidecar
            .entries
            .iter()
            .map(|entry| VectorDocument::new(entry.thought_id.to_string(), entry.vector.clone()))
            .collect();
        let backend = HnswBackend::from_documents(sidecar.metadata.clone(), documents)?;
        Ok(Self::build_from_backend(sidecar, &backend, k, threshold))
    }

    fn build_with_search<F>(
        sidecar: &crate::search::VectorSidecar,
        k: usize,
        threshold: f32,
        mut search: F,
    ) -> Self
    where
        F: FnMut(&[f32], usize) -> Vec<(Uuid, f32)>,
    {
        let mut overlay = Self::new(threshold, k);
        let limit = k.saturating_add(1).max(1);
        for entry in &sidecar.entries {
            let mut neighbors: Vec<ImplicitNeighbor> = search(&entry.vector, limit)
                .into_iter()
                .filter(|(id, score)| *id != entry.thought_id && *score >= threshold)
                .map(|(thought_id, cosine_score)| ImplicitNeighbor {
                    thought_id,
                    cosine_score,
                })
                .collect();
            neighbors.sort_by(|a, b| b.cosine_score.total_cmp(&a.cosine_score));
            neighbors.truncate(k);
            if !neighbors.is_empty() {
                overlay.edges.insert(entry.thought_id, neighbors);
            }
        }
        overlay
    }

    /// Incremental update for one newly appended thought.
    ///
    /// O(N) — called on every append when sidecar is active.
    /// Computes cosine between `new_vector` and all existing entries,
    /// populates `edges[new_id]` and also adds back-edges to neighbors.
    pub fn add_thought(
        &mut self,
        new_id: Uuid,
        new_vector: &[f32],
        sidecar: &crate::search::VectorSidecar,
    ) {
        let mut forward_neighbors: Vec<ImplicitNeighbor> = Vec::new();

        for entry in &sidecar.entries {
            if entry.thought_id == new_id {
                continue; // skip self
            }
            let Some(score) = cosine_similarity(new_vector, &entry.vector) else {
                continue;
            };
            if score < self.threshold {
                continue;
            }

            // Forward edge: new_id → existing
            forward_neighbors.push(ImplicitNeighbor {
                thought_id: entry.thought_id,
                cosine_score: score,
            });

            // Back edge: existing → new_id
            let back_list = self.edges.entry(entry.thought_id).or_default();
            back_list.push(ImplicitNeighbor {
                thought_id: new_id,
                cosine_score: score,
            });
            back_list.sort_by(|a, b| b.cosine_score.total_cmp(&a.cosine_score));
            back_list.truncate(self.k);
        }

        forward_neighbors.sort_by(|a, b| b.cosine_score.total_cmp(&a.cosine_score));
        forward_neighbors.truncate(self.k);
        if !forward_neighbors.is_empty() {
            self.edges.insert(new_id, forward_neighbors);
        }
    }

    /// Serialize to path with atomic rename.
    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        let tmp_path = path.with_extension("tmp");
        let encoded =
            bincode::serde::encode_to_vec(self, bincode::config::standard()).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to encode implicit edge overlay: {e}"),
                )
            })?;
        std::fs::write(&tmp_path, encoded)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Deserialize from path.
    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let (overlay, _) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to decode implicit edge overlay: {e}"),
            )
        })?;
        Ok(overlay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{VectorSidecar, VectorSidecarEntry};

    fn make_sidecar(vectors: Vec<(Uuid, Vec<f32>)>) -> VectorSidecar {
        let entries: Vec<crate::search::VectorSidecarEntry> = vectors
            .into_iter()
            .enumerate()
            .map(|(index, (id, vector))| {
                VectorSidecarEntry::new(id, index as u64, format!("h{index}"), vector)
            })
            .collect();
        VectorSidecar::build(
            "test".to_string(),
            crate::search::EmbeddingMetadata::new("test", 2, "v1"),
            entries.len(),
            Some("head".to_string()),
            chrono::Utc::now(),
            entries,
        )
        .unwrap()
    }

    #[test]
    fn test_build_empty_sidecar() {
        let sidecar = make_sidecar(vec![]);
        let overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.85);
        assert!(overlay.edges.is_empty());
    }

    #[test]
    fn test_build_identical_thoughts() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let sidecar = make_sidecar(vec![(id_a, vec![1.0, 0.0]), (id_b, vec![1.0, 0.0])]);
        let overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.99);
        assert_eq!(overlay.edges.len(), 2);
        let a_neighbors = overlay.edges.get(&id_a).unwrap();
        assert_eq!(a_neighbors.len(), 1);
        assert_eq!(a_neighbors[0].thought_id, id_b);
        assert!(a_neighbors[0].cosine_score >= 0.999);
    }

    #[test]
    fn test_threshold_gate() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let sidecar = make_sidecar(vec![(id_a, vec![1.0, 0.0]), (id_b, vec![0.5, 0.5])]);
        let overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.1);
        assert!(!overlay.edges.is_empty());

        let overlay_strict = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.99);
        assert!(overlay_strict.edges.is_empty());
    }

    #[test]
    fn test_k_limit() {
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let sidecar = make_sidecar(vec![
            (ids[0], vec![1.0, 0.0]),
            (ids[1], vec![0.99, 0.01]),
            (ids[2], vec![0.98, 0.02]),
            (ids[3], vec![0.97, 0.03]),
            (ids[4], vec![0.96, 0.04]),
        ]);
        let overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 2, 0.5);
        for neighbors in overlay.edges.values() {
            assert!(neighbors.len() <= 2);
        }
    }

    #[test]
    fn test_add_thought_incremental() {
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        let sidecar = make_sidecar(vec![(ids[0], vec![1.0, 0.0]), (ids[1], vec![0.99, 0.01])]);
        let mut overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.5);

        let extended_sidecar = make_sidecar(vec![
            (ids[0], vec![1.0, 0.0]),
            (ids[1], vec![0.99, 0.01]),
            (ids[2], vec![0.98, 0.02]),
        ]);
        overlay.add_thought(ids[2], &[0.98, 0.02], &extended_sidecar);

        let from_scratch = ImplicitEdgeOverlay::build_from_sidecar(&extended_sidecar, 5, 0.5);
        assert_eq!(overlay.edges, from_scratch.edges);
    }

    #[test]
    fn test_back_edges() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let sidecar = make_sidecar(vec![(id_a, vec![1.0, 0.0])]);
        let mut overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.5);
        let extended_sidecar = make_sidecar(vec![(id_a, vec![1.0, 0.0]), (id_b, vec![0.99, 0.01])]);
        overlay.add_thought(id_b, &[0.99, 0.01], &extended_sidecar);

        let a_neighbors = overlay.edges.get(&id_a).unwrap();
        assert_eq!(a_neighbors.len(), 1);
        assert_eq!(a_neighbors[0].thought_id, id_b);
    }

    #[test]
    fn test_roundtrip_persistence() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let sidecar = make_sidecar(vec![(id_a, vec![1.0, 0.0]), (id_b, vec![0.99, 0.01])]);
        let overlay = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, 5, 0.5);

        let tmp_dir = std::env::temp_dir();
        let path = tmp_dir.join(format!("mentisdb_auto_edges_test_{}.bin", Uuid::new_v4()));
        overlay.save_to_path(&path).unwrap();
        let loaded = ImplicitEdgeOverlay::load_from_path(&path).unwrap();
        assert_eq!(overlay.edges, loaded.edges);
        assert_eq!(overlay.threshold, loaded.threshold);
        assert_eq!(overlay.k, loaded.k);
        let _ = std::fs::remove_file(&path);
    }
}
