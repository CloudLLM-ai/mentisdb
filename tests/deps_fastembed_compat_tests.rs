//! Guards persisted vector-sidecar compatibility across fastembed upgrades:
//! model id, dimension and the numeric embedding of a fixed text must not drift.

#![cfg(feature = "local-embeddings")]

use mentisdb::search::fastembed_provider::{
    FastEmbedProvider, FASTEMBED_MINILM_DIMENSION, FASTEMBED_MINILM_MODEL_ID,
};
use mentisdb::search::{EmbeddingInput, EmbeddingProvider};

/// First eight components of the AllMiniLML6V2 embedding of "hello world",
/// captured with fastembed 5.x; fastembed 7.x must reproduce them.
const HELLO_WORLD_HEAD: [f32; 8] = [
    -0.03447729,
    0.031023258,
    0.0067350096,
    0.026108995,
    -0.039362054,
    -0.16030267,
    0.066924,
    -0.0064414404,
];

/// Model identity and the embedding of a fixed text stay stable.
#[test]
fn test_fastembed_minilm_identity_and_vector_are_stable() {
    let provider = FastEmbedProvider::try_new().expect("model load");
    assert_eq!(provider.metadata().model_id, FASTEMBED_MINILM_MODEL_ID);
    assert_eq!(provider.metadata().dimension, FASTEMBED_MINILM_DIMENSION);
    let out = provider
        .embed_batch(&[EmbeddingInput::new("a", "hello world")])
        .expect("embed");
    assert_eq!(out[0].values.len(), FASTEMBED_MINILM_DIMENSION);
    for (got, want) in out[0].values.iter().zip(HELLO_WORLD_HEAD) {
        assert!((got - want).abs() < 1e-4, "got {got} want {want}");
    }
}
