//! Real semantic embedding provider backed by the `fastembed` crate.
//!
//! This module is only compiled when the `local-embeddings` feature is enabled.
//! It downloads an ~80 MB ONNX model on first use (AllMiniLML6V2) and produces
//! 384-dimensional semantic embeddings locally — no API calls, no keys.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use super::{EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector};

/// Stable provider identifier for the fastembed AllMiniLML6V2 provider.
pub const FASTEMBED_MINILM_MODEL_ID: &str = "fastembed-all-minilm-l6-v2";

/// Output vector dimension for AllMiniLML6V2.
pub const FASTEMBED_MINILM_DIMENSION: usize = 384;

/// Version label for the AllMiniLML6V2 embedding space.
pub const FASTEMBED_MINILM_VERSION: &str = "1";

/// Opaque string-backed error for `fastembed` failures.
///
/// `fastembed` returns `anyhow::Result`, and `anyhow` is not a direct
/// dependency of this crate. We capture the error message as a `String` so
/// the type satisfies `std::error::Error + Send + Sync + 'static` without
/// depending on `anyhow` directly.
#[derive(Debug)]
pub struct FastEmbedError(String);

impl fmt::Display for FastEmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fastembed error: {}", self.0)
    }
}

impl std::error::Error for FastEmbedError {}

/// Local semantic embedding provider using `fastembed` AllMiniLML6V2.
///
/// Wraps `TextEmbedding` in a `Mutex` because fastembed v5's `embed` method
/// takes `&mut self`, while `EmbeddingProvider::embed_batch` takes `&self`.
/// The mutex makes concurrent calls safe while preserving the shared-reference
/// interface.
///
/// # Examples
///
/// ```rust,no_run
/// use mentisdb::search::{EmbeddingInput, EmbeddingProvider};
/// use mentisdb::search::fastembed_provider::FastEmbedProvider;
///
/// let provider = FastEmbedProvider::try_new().expect("model load failed");
/// let inputs = vec![EmbeddingInput::new("doc-1", "hello world")];
/// let vectors = provider.embed_batch(&inputs).expect("embed failed");
/// assert_eq!(vectors[0].values.len(), 384);
/// ```
pub struct FastEmbedProvider {
    model: SharedModel,
    metadata: EmbeddingMetadata,
}

/// Process-wide handle to one loaded ONNX session.
type SharedModel = Arc<Mutex<TextEmbedding>>;

/// Registry of loaded models keyed by stable model id.
///
/// Every chain used to build its own `TextEmbedding` (about 100 MB of ORT
/// session each, never released); sharing one session per model id keeps the
/// process footprint flat regardless of how many chains are open.
fn model_registry() -> &'static Mutex<HashMap<&'static str, SharedModel>> {
    static REGISTRY: OnceLock<Mutex<HashMap<&'static str, SharedModel>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return the shared model for `model_id`, loading it with `load` on first use.
///
/// The registry lock is held while loading so concurrent first callers load
/// the model exactly once.
fn shared_model(
    model_id: &'static str,
    load: impl FnOnce() -> Result<TextEmbedding, FastEmbedError>,
) -> Result<SharedModel, FastEmbedError> {
    let mut registry = model_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = registry.get(model_id) {
        return Ok(Arc::clone(existing));
    }
    let loaded = Arc::new(Mutex::new(load()?));
    registry.insert(model_id, Arc::clone(&loaded));
    Ok(loaded)
}

impl fmt::Debug for FastEmbedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FastEmbedProvider")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl FastEmbedProvider {
    /// Returns `true` when both providers use the same loaded ONNX session.
    pub fn shares_model_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.model, &other.model)
    }

    /// Obtain a provider backed by the process-wide AllMiniLML6V2 session.
    ///
    /// The model is loaded once per process; later calls return a cheap handle.
    /// Downloads the model (~80 MB) from Hugging Face on first use and caches
    /// it locally. Subsequent calls load from cache and are fast.
    ///
    /// # Errors
    ///
    /// Returns `FastEmbedError` when the model cannot be downloaded or the
    /// ONNX runtime fails to initialise.
    pub fn try_new() -> Result<Self, FastEmbedError> {
        let model = shared_model(FASTEMBED_MINILM_MODEL_ID, || {
            TextEmbedding::try_new(
                InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
            )
            .map_err(|e| FastEmbedError(e.to_string()))
        })?;
        Ok(Self {
            model,
            metadata: EmbeddingMetadata::new(
                FASTEMBED_MINILM_MODEL_ID,
                FASTEMBED_MINILM_DIMENSION,
                FASTEMBED_MINILM_VERSION,
            ),
        })
    }
}

impl EmbeddingProvider for FastEmbedProvider {
    type Error = FastEmbedError;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    /// Embed a batch of inputs using AllMiniLML6V2.
    ///
    /// # Errors
    ///
    /// Returns `FastEmbedError` when the ONNX runtime fails during inference,
    /// or when the internal mutex is poisoned (a previous call panicked).
    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, Self::Error> {
        let texts: Vec<&str> = inputs.iter().map(|i| i.text.as_str()).collect();
        let mut model = self
            .model
            .lock()
            .map_err(|e| FastEmbedError(format!("fastembed mutex poisoned: {e}")))?;
        let embeddings = model
            .embed(texts, None)
            .map_err(|e| FastEmbedError(e.to_string()))?;
        Ok(embeddings.into_iter().map(EmbeddingVector::new).collect())
    }
}
