//! Embedding-based nearest-neighbor synonym generator.
//!
//! Uses the built-in [`LocalTextEmbeddingProvider`] to embed query terms and
//! candidate vocabulary words, then finds cosine-similar terms at query time.
//! This is fully local and deterministic.

use crate::search::{
    EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, LocalTextEmbeddingProvider,
    VectorDocument, VectorIndex, VectorQuery,
};
use std::collections::HashMap;

/// Synonym generator backed by vector similarity over a fixed vocabulary.
///
/// The generator embeds a caller-provided vocabulary at construction time,
/// then answers synonym lookups via cosine-similar nearest neighbors.
#[derive(Debug, Clone)]
pub struct EmbeddingSynonymGenerator {
    index: VectorIndex,
}

impl EmbeddingSynonymGenerator {
    /// Build a generator from a vocabulary of candidate words.
    ///
    /// Each word is embedded with the built-in local text provider. The
    /// vocabulary should include all terms that might serve as synonyms.
    /// Duplicate words are deduplicated by their string representation.
    pub fn from_vocabulary(vocabulary: &[String]) -> Self {
        let provider = LocalTextEmbeddingProvider::new();
        let metadata = provider.metadata().clone();

        let inputs: Vec<EmbeddingInput> = vocabulary
            .iter()
            .map(|word| EmbeddingInput::new(word.clone(), word.clone()))
            .collect();

        let vectors = provider.embed_batch(&inputs).unwrap_or_default();
        let documents: Vec<VectorDocument> = inputs
            .into_iter()
            .zip(vectors)
            .map(|(input, vector)| VectorDocument::new(input.input_id, vector.values))
            .collect();

        let index = VectorIndex::from_documents(metadata, documents).unwrap_or_else(|_| {
            VectorIndex::new(EmbeddingMetadata::new(
                crate::search::LOCAL_TEXT_EMBEDDING_MODEL_ID,
                crate::search::LOCAL_TEXT_EMBEDDING_DIMENSION,
                crate::search::LOCAL_TEXT_EMBEDDING_VERSION,
            ))
        });

        Self { index }
    }

    /// Return up to `k` cosine-nearest terms for `term`, excluding `term` itself.
    pub fn lookup(&self, term: &str, k: usize) -> Vec<String> {
        let provider = LocalTextEmbeddingProvider::new();
        let query_vector = provider
            .embed_batch(&[EmbeddingInput::new("__query__", term)])
            .ok()
            .and_then(|mut v| v.pop())
            .map(|v| v.values)
            .unwrap_or_default();

        if query_vector.is_empty() {
            return Vec::new();
        }

        let query = VectorQuery::new(query_vector).with_limit(k + 1);
        let hits = self.index.search(&query).unwrap_or_default();

        hits.into_iter()
            .filter(|hit| hit.document_id != term)
            .take(k)
            .map(|hit| hit.document_id)
            .collect()
    }

    /// Expand every normalized token in `text` into a synonym map.
    ///
    /// Only tokens that have at least one neighbor in the vocabulary are
    /// included. The map value is the list of nearest neighbors.
    pub fn expand_text(&self, text: &str, k: usize) -> HashMap<String, Vec<String>> {
        let mut result = HashMap::new();
        for term in crate::search::lexical::normalize_lexical_tokens(text, false) {
            let syms = self.lookup(&term, k);
            if !syms.is_empty() {
                result.insert(term, syms);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_generator_finds_similar_words() {
        let vocab = vec![
            "fast".into(),
            "quick".into(),
            "rapid".into(),
            "slow".into(),
            "sluggish".into(),
            "search".into(),
            "lookup".into(),
            "find".into(),
        ];
        let gen = EmbeddingSynonymGenerator::from_vocabulary(&vocab);

        let syms = gen.lookup("fast", 3);
        assert!(
            syms.contains(&"quick".into()) || syms.contains(&"rapid".into()),
            "expected quick or rapid as synonym for fast, got {:?}",
            syms
        );
    }

    #[test]
    fn embedding_generator_excludes_self() {
        let vocab = vec!["fast".into(), "quick".into()];
        let gen = EmbeddingSynonymGenerator::from_vocabulary(&vocab);
        let syms = gen.lookup("fast", 1);
        assert!(!syms.contains(&"fast".into()));
    }

    #[test]
    fn embedding_generator_does_not_return_unknown() {
        let vocab = vec!["fast".into(), "quick".into(), "slow".into(), "rapid".into()];
        let gen = EmbeddingSynonymGenerator::from_vocabulary(&vocab);
        let syms = gen.lookup("xyz_unknown", 3);
        assert!(!syms.contains(&"xyz_unknown".into()));
    }
}
