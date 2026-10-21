//! Verifies that `FastEmbedProvider` handles share one process-wide model.

#![cfg(feature = "local-embeddings")]

use mentisdb::search::fastembed_provider::FastEmbedProvider;
use mentisdb::search::{EmbeddingInput, EmbeddingProvider};

/// Two providers created in one process must reuse the same ONNX session and
/// both remain usable.
#[test]
fn test_fastembed_providers_share_one_model() {
    let a = FastEmbedProvider::try_new().expect("model load");
    let b = FastEmbedProvider::try_new().expect("shared handle");
    assert!(a.shares_model_with(&b));
    let input = [EmbeddingInput::new("x", "shared model")];
    assert_eq!(
        a.embed_batch(&input).unwrap()[0].values,
        b.embed_batch(&input).unwrap()[0].values
    );
}
