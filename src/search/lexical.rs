//! Versioned lexical indexing and BM25-style scoring for committed thoughts.
//!
//! The lexical engine is derived state only. It never mutates the durable
//! chain, and it can be rebuilt at any time from a slice of committed
//! [`Thought`](crate::Thought) records.

use crate::{AgentRegistry, Thought};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Current lexical index format version.
pub const LEXICAL_INDEX_FORMAT_VERSION: u32 = 1;

/// Current lexical tokenizer and normalizer version.
/// Bumped to 2 when Porter stemming was added to token normalization.
pub const LEXICAL_NORMALIZER_VERSION: u32 = 2;

/// Logical lexical fields that contribute to the derived index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LexicalField {
    /// The thought's primary content text.
    Content,
    /// The thought's caller-supplied tags.
    Tags,
    /// The thought's semantic concepts.
    Concepts,
    /// The producing agent's stable id.
    AgentId,
    /// The producing agent's registry metadata.
    AgentRegistry,
}

/// Metadata describing one rebuildable lexical index snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalIndexMetadata {
    /// Lexical index format version.
    pub index_format_version: u32,
    /// Tokenizer and normalizer version.
    pub normalizer_version: u32,
    /// Number of indexed thoughts.
    pub thought_count: usize,
    /// Chain head hash observed when the index was built, if any.
    pub head_hash: Option<String>,
}

impl LexicalIndexMetadata {
    /// Return whether this metadata matches the current lexical schema versions.
    pub fn is_current_format(&self) -> bool {
        self.index_format_version == LEXICAL_INDEX_FORMAT_VERSION
            && self.normalizer_version == LEXICAL_NORMALIZER_VERSION
    }

    /// Return whether this metadata still matches the provided thought slice.
    ///
    /// This is intended for callers that persist lexical derived state
    /// separately and need a cheap rebuild check.
    pub fn matches_thoughts(&self, thoughts: &[Thought]) -> bool {
        self.is_current_format()
            && self.thought_count == thoughts.len()
            && self.head_hash == thoughts.last().map(|thought| thought.hash.clone())
    }
}

/// Per-document lexical statistics derived from one committed thought.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalDocumentStats {
    /// Zero-based position of the thought within the input slice.
    pub doc_position: usize,
    /// Durable append-order thought index.
    pub thought_index: u64,
    /// Stable thought UUID.
    pub thought_id: Uuid,
    /// Commit timestamp of the thought.
    pub timestamp: DateTime<Utc>,
    /// Importance score copied from the thought for tie-breaking.
    pub importance: f32,
    /// Confidence score copied from the thought for tie-breaking.
    pub confidence: Option<f32>,
    /// Normalized token count derived from `content`.
    pub content_len: u32,
    /// Normalized token count derived from `tags`.
    pub tag_len: u32,
    /// Normalized token count derived from `concepts`.
    pub concept_len: u32,
    /// Normalized token count derived from `agent_id`.
    pub agent_id_len: u32,
    /// Normalized token count derived from agent registry text.
    pub agent_registry_len: u32,
    /// Total normalized token count across all indexed fields.
    pub total_len: u32,
}

impl LexicalDocumentStats {
    /// Return the normalized token count for one field.
    pub fn field_len(&self, field: LexicalField) -> u32 {
        match field {
            LexicalField::Content => self.content_len,
            LexicalField::Tags => self.tag_len,
            LexicalField::Concepts => self.concept_len,
            LexicalField::AgentId => self.agent_id_len,
            LexicalField::AgentRegistry => self.agent_registry_len,
        }
    }
}

/// One posting entry for a single normalized term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalPosting {
    /// Zero-based position of the thought within the input slice.
    pub doc_position: usize,
    /// Term frequency contributed by `content`.
    pub content_term_frequency: u32,
    /// Term frequency contributed by `tags`.
    pub tag_term_frequency: u32,
    /// Term frequency contributed by `concepts`.
    pub concept_term_frequency: u32,
    /// Term frequency contributed by `agent_id`.
    pub agent_id_term_frequency: u32,
    /// Term frequency contributed by agent registry text.
    pub agent_registry_term_frequency: u32,
}

impl LexicalPosting {
    /// Return the frequency contributed by one field.
    pub fn term_frequency(&self, field: LexicalField) -> u32 {
        match field {
            LexicalField::Content => self.content_term_frequency,
            LexicalField::Tags => self.tag_term_frequency,
            LexicalField::Concepts => self.concept_term_frequency,
            LexicalField::AgentId => self.agent_id_term_frequency,
            LexicalField::AgentRegistry => self.agent_registry_term_frequency,
        }
    }

    /// Return the total frequency across all indexed fields.
    pub fn total_term_frequency(&self) -> u32 {
        self.content_term_frequency
            + self.tag_term_frequency
            + self.concept_term_frequency
            + self.agent_id_term_frequency
            + self.agent_registry_term_frequency
    }
}

/// BM25-style scoring parameters for lexical ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalScoringConfig {
    /// BM25 saturation parameter.
    pub k1: f32,
    /// BM25 length-normalization parameter.
    pub b: f32,
    /// Relative weight for content matches.
    pub content_weight: f32,
    /// Relative weight for tag matches.
    pub tag_weight: f32,
    /// Relative weight for concept matches.
    pub concept_weight: f32,
    /// Relative weight for agent-id matches.
    pub agent_id_weight: f32,
    /// Relative weight for agent-registry text matches.
    pub agent_registry_weight: f32,
}

impl Default for LexicalScoringConfig {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            content_weight: 1.0,
            tag_weight: 1.6,
            concept_weight: 1.4,
            agent_id_weight: 1.5,
            agent_registry_weight: 1.1,
        }
    }
}

/// Indexed field types that contributed to one lexical hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LexicalMatchSource {
    /// Match derived from the thought content.
    Content,
    /// Match derived from the thought tags.
    Tags,
    /// Match derived from the thought concepts.
    Concepts,
    /// Match derived from the thought agent id.
    AgentId,
    /// Match derived from registered agent metadata.
    AgentRegistry,
}

impl LexicalMatchSource {
    /// Return the stable lowercase name of this match source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Tags => "tags",
            Self::Concepts => "concepts",
            Self::AgentId => "agent_id",
            Self::AgentRegistry => "agent_registry",
        }
    }
}

/// Ranked lexical query parameters.
#[derive(Debug, Clone, PartialEq)]
/// Lexical (BM25) query for searching thought content, tags, concepts,
/// and agent metadata.
///
/// This is the lower-level building block for ranked search. Most custom
/// integrations should prefer [`RankedSearchQuery`](crate::RankedSearchQuery),
/// which automatically wires lexical search together with vector and graph
/// signals. Use `LexicalQuery` directly when you only need text matching
/// and do not want the full hybrid pipeline.
///
/// # Example
///
/// ```rust
/// use mentisdb::search::lexical::LexicalQuery;
///
/// let query = LexicalQuery::new("cache invalidation strategy")
///     .with_limit(10);
/// ```
pub struct LexicalQuery {
    /// Raw query text that will be normalized using the current normalizer.
    pub text: String,
    /// Optional maximum number of ranked hits to return.
    pub limit: Option<usize>,
    /// BM25-style scoring configuration.
    pub scoring: LexicalScoringConfig,
    /// Per-field document-frequency cutoffs for stop-word suppression.
    pub df_cutoffs: Bm25DfCutoffs,
    /// Optional synonym expansion map: each original term maps to a list of
    /// synonyms that should also be searched with reduced weight.
    pub synonyms: std::collections::HashMap<String, Vec<String>>,
    /// Weight applied to synonym matches relative to original term matches.
    pub synonym_weight: f32,
    /// Normalized (stemmed) synonym terms keyed by normalized query term,
    /// built once by [`LexicalQuery::with_synonyms`].
    normalized_synonyms: Option<HashMap<String, Vec<String>>>,
}

/// Maximum number of normalized synonym terms added per query term.
///
/// Caps term explosion for headwords with long synonym lists: every extra
/// term costs a posting-list scan.
pub const MAX_SYNONYMS_PER_TERM: usize = 5;

/// Normalize a synonym map so lookups can be keyed by the normalized
/// (Porter-stemmed, lemma-expanded) query term.
///
/// Keys are visited in sorted order so merging several raw keys that share a
/// normalized form is deterministic. The original key is kept as well, so
/// maps that were already keyed by stemmed terms keep working. Each entry is
/// deduplicated and capped at [`MAX_SYNONYMS_PER_TERM`] normalized terms.
fn normalize_synonym_map(synonyms: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    let mut keys: Vec<&String> = synonyms.keys().collect();
    keys.sort();
    let mut normalized: HashMap<String, Vec<String>> = HashMap::new();
    for key in keys {
        let terms: Vec<String> = synonyms[key]
            .iter()
            .flat_map(|synonym| unique_normalized_terms(synonym))
            .collect();
        let mut targets = unique_normalized_terms(key);
        targets.push(key.clone());
        for target in targets {
            let entry = normalized.entry(target).or_default();
            for term in &terms {
                if entry.len() < MAX_SYNONYMS_PER_TERM && !entry.contains(term) {
                    entry.push(term.clone());
                }
            }
        }
    }
    normalized
}

impl LexicalQuery {
    /// Create a new lexical query with default scoring settings.
    ///
    /// # Example
    ///
    /// ```rust
    /// use mentisdb::search::lexical::LexicalQuery;
    /// let query = LexicalQuery::new("deployment rollback");
    /// ```
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: None,
            scoring: LexicalScoringConfig::default(),
            df_cutoffs: Bm25DfCutoffs::default(),
            synonyms: std::collections::HashMap::new(),
            synonym_weight: 0.7,
            normalized_synonyms: None,
        }
    }

    /// Enable synonym expansion with a custom map and weight.
    pub fn with_synonyms(
        mut self,
        synonyms: std::collections::HashMap<String, Vec<String>>,
        weight: f32,
    ) -> Self {
        self.normalized_synonyms = Some(normalize_synonym_map(&synonyms));
        self.synonyms = synonyms;
        self.synonym_weight = weight.max(0.0);
        self
    }

    /// Limit the number of ranked hits returned.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Replace the scoring configuration used for this query.
    pub fn with_scoring(mut self, scoring: LexicalScoringConfig) -> Self {
        self.scoring = scoring;
        self
    }

    /// Replace the per-field document-frequency cutoffs.
    pub fn with_df_cutoffs(mut self, cutoffs: Bm25DfCutoffs) -> Self {
        self.df_cutoffs = cutoffs;
        self
    }

    /// Return the unique normalized query terms in encounter order.
    pub fn normalized_terms(&self) -> Vec<String> {
        unique_normalized_terms(&self.text)
    }

    /// Return original terms and their synonyms for searching.
    ///
    /// Synonym lookup is keyed by the *normalized* query term, so an inflected
    /// query such as "debugging" (stem "debug") finds the thesaurus entry for
    /// "debug". Synonym terms are normalized once in
    /// [`LexicalQuery::with_synonyms`] and capped at
    /// [`MAX_SYNONYMS_PER_TERM`] per query term. If `synonyms` was assigned
    /// directly instead of through `with_synonyms`, it is normalized on the
    /// fly.
    pub fn expanded_terms(&self) -> Vec<(String, f32)> {
        let computed;
        let normalized_synonyms = match &self.normalized_synonyms {
            Some(map) => map,
            None if self.synonyms.is_empty() => {
                computed = HashMap::new();
                &computed
            }
            None => {
                computed = normalize_synonym_map(&self.synonyms);
                &computed
            }
        };
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for term in self.normalized_terms() {
            if seen.insert(term.clone()) {
                result.push((term.clone(), 1.0));
                if let Some(syms) = normalized_synonyms.get(&term) {
                    for synonym in syms {
                        if seen.insert(synonym.clone()) {
                            result.push((synonym.clone(), self.synonym_weight));
                        }
                    }
                }
            }
        }
        result
    }
}

/// Per-field BM25 document-frequency cutoffs.
///
/// Terms whose **global** document frequency (across all fields) exceeds
/// `cutoff_ratio * N` are skipped for that field only. Using global DF
/// preserves the original stop-word suppression behavior: a term that
/// appears in many documents (in any field) is too common to be
/// discriminative for low-cutoff fields like content, but may still
/// contribute to higher-cutoff fields like agent-id or agent-registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Bm25DfCutoffs {
    /// DF cutoff ratio for content matches.
    pub content: f32,
    /// DF cutoff ratio for tag matches.
    pub tags: f32,
    /// DF cutoff ratio for concept matches.
    pub concepts: f32,
    /// DF cutoff ratio for agent-id matches.
    pub agent_id: f32,
    /// DF cutoff ratio for agent-registry text matches.
    pub agent_registry: f32,
}

impl Default for Bm25DfCutoffs {
    fn default() -> Self {
        Self {
            content: 0.30,
            tags: 0.30,
            concepts: 0.30,
            agent_id: 0.70,
            agent_registry: 0.60,
        }
    }
}

impl Bm25DfCutoffs {
    /// Return the cutoff ratio for one field.
    pub fn for_field(&self, field: LexicalField) -> f32 {
        match field {
            LexicalField::Content => self.content,
            LexicalField::Tags => self.tags,
            LexicalField::Concepts => self.concepts,
            LexicalField::AgentId => self.agent_id,
            LexicalField::AgentRegistry => self.agent_registry,
        }
    }
}

/// One ranked lexical hit.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalHit {
    /// Zero-based position of the thought within the indexed slice.
    pub doc_position: usize,
    /// Durable append-order thought index.
    pub thought_index: u64,
    /// Stable thought UUID.
    pub thought_id: Uuid,
    /// Final BM25-style score after field weighting.
    pub score: f32,
    /// Unique normalized query terms that matched this hit.
    pub matched_terms: Vec<String>,
    /// Indexed field sources that contributed to the score.
    pub match_sources: Vec<LexicalMatchSource>,
}

/// Derived lexical index built from committed thoughts.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalIndex {
    metadata: LexicalIndexMetadata,
    document_stats: Vec<LexicalDocumentStats>,
    postings: HashMap<String, Vec<LexicalPosting>>,
    average_content_len: f32,
    average_tag_len: f32,
    average_concept_len: f32,
    average_agent_id_len: f32,
    average_agent_registry_len: f32,
}

impl LexicalIndex {
    /// Build a rebuildable lexical index from committed thoughts.
    pub fn build(thoughts: &[Thought]) -> Self {
        let empty_registry = AgentRegistry::default();
        Self::build_with_registry(thoughts, &empty_registry)
    }

    /// Build a rebuildable lexical index from committed thoughts plus agent metadata.
    pub fn build_with_registry(thoughts: &[Thought], registry: &AgentRegistry) -> Self {
        let mut document_stats = Vec::with_capacity(thoughts.len());
        let mut postings = HashMap::<String, Vec<LexicalPosting>>::new();
        let mut total_content_len = 0_u64;
        let mut total_tag_len = 0_u64;
        let mut total_concept_len = 0_u64;
        let mut total_agent_id_len = 0_u64;
        let mut total_agent_registry_len = 0_u64;

        for (doc_position, thought) in thoughts.iter().enumerate() {
            let content_tokens = normalize_lexical_tokens(&thought.content, false);
            let tag_tokens = thought
                .tags
                .iter()
                .flat_map(|tag| normalize_lexical_tokens(tag, false))
                .collect::<Vec<_>>();
            let concept_tokens = thought
                .concepts
                .iter()
                .flat_map(|concept| normalize_lexical_tokens(concept, false))
                .collect::<Vec<_>>();
            let agent_id_tokens = normalize_lexical_tokens(&thought.agent_id, false);
            let agent_registry_tokens = registry
                .agents
                .get(&thought.agent_id)
                .map(agent_registry_tokens)
                .unwrap_or_default();

            let content_len = content_tokens.len() as u32;
            let tag_len = tag_tokens.len() as u32;
            let concept_len = concept_tokens.len() as u32;
            let agent_id_len = agent_id_tokens.len() as u32;
            let agent_registry_len = agent_registry_tokens.len() as u32;
            total_content_len += u64::from(content_len);
            total_tag_len += u64::from(tag_len);
            total_concept_len += u64::from(concept_len);
            total_agent_id_len += u64::from(agent_id_len);
            total_agent_registry_len += u64::from(agent_registry_len);

            document_stats.push(LexicalDocumentStats {
                doc_position,
                thought_index: thought.index,
                thought_id: thought.id,
                timestamp: thought.timestamp,
                importance: thought.importance,
                confidence: thought.confidence,
                content_len,
                tag_len,
                concept_len,
                agent_id_len,
                agent_registry_len,
                total_len: content_len + tag_len + concept_len + agent_id_len + agent_registry_len,
            });

            let mut frequencies = HashMap::<String, LexicalPosting>::new();
            observe_tokens(
                doc_position,
                &content_tokens,
                LexicalField::Content,
                &mut frequencies,
            );
            observe_tokens(
                doc_position,
                &tag_tokens,
                LexicalField::Tags,
                &mut frequencies,
            );
            observe_tokens(
                doc_position,
                &concept_tokens,
                LexicalField::Concepts,
                &mut frequencies,
            );
            observe_tokens(
                doc_position,
                &agent_id_tokens,
                LexicalField::AgentId,
                &mut frequencies,
            );
            observe_tokens(
                doc_position,
                &agent_registry_tokens,
                LexicalField::AgentRegistry,
                &mut frequencies,
            );

            for (term, posting) in frequencies {
                postings.entry(term).or_default().push(posting);
            }
        }

        for entries in postings.values_mut() {
            entries.sort_by_key(|posting| posting.doc_position);
        }

        let thought_count = thoughts.len();
        let metadata = LexicalIndexMetadata {
            index_format_version: LEXICAL_INDEX_FORMAT_VERSION,
            normalizer_version: LEXICAL_NORMALIZER_VERSION,
            thought_count,
            head_hash: thoughts.last().map(|thought| thought.hash.clone()),
        };

        Self {
            metadata,
            document_stats,
            postings,
            average_content_len: average_length(total_content_len, thought_count),
            average_tag_len: average_length(total_tag_len, thought_count),
            average_concept_len: average_length(total_concept_len, thought_count),
            average_agent_id_len: average_length(total_agent_id_len, thought_count),
            average_agent_registry_len: average_length(total_agent_registry_len, thought_count),
        }
    }

    /// Create an empty lexical index ready for incremental observation.
    pub fn empty() -> Self {
        Self {
            metadata: LexicalIndexMetadata {
                index_format_version: LEXICAL_INDEX_FORMAT_VERSION,
                normalizer_version: LEXICAL_NORMALIZER_VERSION,
                thought_count: 0,
                head_hash: None,
            },
            document_stats: Vec::new(),
            postings: HashMap::new(),
            average_content_len: 0.0,
            average_tag_len: 0.0,
            average_concept_len: 0.0,
            average_agent_id_len: 0.0,
            average_agent_registry_len: 0.0,
        }
    }

    /// Incrementally add one thought to the lexical index.
    pub fn observe(&mut self, doc_position: usize, thought: &Thought, registry: &AgentRegistry) {
        let content_tokens = normalize_lexical_tokens(&thought.content, false);
        let tag_tokens = thought
            .tags
            .iter()
            .flat_map(|tag| normalize_lexical_tokens(tag, false))
            .collect::<Vec<_>>();
        let concept_tokens = thought
            .concepts
            .iter()
            .flat_map(|concept| normalize_lexical_tokens(concept, false))
            .collect::<Vec<_>>();
        let agent_id_tokens = normalize_lexical_tokens(&thought.agent_id, false);
        let agent_registry_tokens = registry
            .agents
            .get(&thought.agent_id)
            .map(agent_registry_tokens)
            .unwrap_or_default();

        let content_len = content_tokens.len() as u32;
        let tag_len = tag_tokens.len() as u32;
        let concept_len = concept_tokens.len() as u32;
        let agent_id_len = agent_id_tokens.len() as u32;
        let agent_registry_len = agent_registry_tokens.len() as u32;

        let n = self.document_stats.len() as f32;
        self.average_content_len = (self.average_content_len * n + content_len as f32) / (n + 1.0);
        self.average_tag_len = (self.average_tag_len * n + tag_len as f32) / (n + 1.0);
        self.average_concept_len = (self.average_concept_len * n + concept_len as f32) / (n + 1.0);
        self.average_agent_id_len =
            (self.average_agent_id_len * n + agent_id_len as f32) / (n + 1.0);
        self.average_agent_registry_len =
            (self.average_agent_registry_len * n + agent_registry_len as f32) / (n + 1.0);

        self.document_stats.push(LexicalDocumentStats {
            doc_position,
            thought_index: thought.index,
            thought_id: thought.id,
            timestamp: thought.timestamp,
            importance: thought.importance,
            confidence: thought.confidence,
            content_len,
            tag_len,
            concept_len,
            agent_id_len,
            agent_registry_len,
            total_len: content_len + tag_len + concept_len + agent_id_len + agent_registry_len,
        });

        let mut frequencies = HashMap::<String, LexicalPosting>::new();
        observe_tokens(
            doc_position,
            &content_tokens,
            LexicalField::Content,
            &mut frequencies,
        );
        observe_tokens(
            doc_position,
            &tag_tokens,
            LexicalField::Tags,
            &mut frequencies,
        );
        observe_tokens(
            doc_position,
            &concept_tokens,
            LexicalField::Concepts,
            &mut frequencies,
        );
        observe_tokens(
            doc_position,
            &agent_id_tokens,
            LexicalField::AgentId,
            &mut frequencies,
        );
        observe_tokens(
            doc_position,
            &agent_registry_tokens,
            LexicalField::AgentRegistry,
            &mut frequencies,
        );

        for (term, posting) in frequencies {
            self.postings.entry(term).or_default().push(posting);
        }

        self.metadata.thought_count = self.document_stats.len();
        self.metadata.head_hash = Some(thought.hash.clone());
    }

    /// Return metadata describing this lexical index snapshot.
    pub fn metadata(&self) -> &LexicalIndexMetadata {
        &self.metadata
    }

    /// Return the number of indexed documents.
    pub fn document_count(&self) -> usize {
        self.document_stats.len()
    }

    /// Return the number of unique normalized terms in the index.
    pub fn term_count(&self) -> usize {
        self.postings.len()
    }

    /// Estimate the RAM footprint of this in-memory lexical index in bytes.
    ///
    /// This is a lower-bound approximation that counts the data stored in the
    /// postings map, document-stats vector, and metadata. HashMap and Vec
    /// capacity overhead are not fully accounted for, so the real heap size
    /// may be 1.2–1.5× larger.
    pub fn estimated_memory_bytes(&self) -> u64 {
        use std::mem::size_of;

        // Metadata is tiny.
        let metadata = 64u64;

        // One stat block per indexed thought.
        let doc_stats =
            (self.document_stats.len() as u64) * (size_of::<LexicalDocumentStats>() as u64);

        // Postings: term string + HashMap entry overhead + Vec overhead + postings.
        let mut postings = 0u64;
        for (term, list) in &self.postings {
            postings += term.len() as u64; // string bytes
            postings += 32; // HashMap entry overhead estimate
            postings += 24; // Vec header/capacity estimate
            postings += (list.len() as u64) * (size_of::<LexicalPosting>() as u64);
        }

        metadata + doc_stats + postings
    }

    /// Return per-document statistics in slice order.
    pub fn document_stats(&self) -> &[LexicalDocumentStats] {
        &self.document_stats
    }

    /// Return the average normalized token count for one indexed field.
    pub fn average_field_length(&self, field: LexicalField) -> f32 {
        match field {
            LexicalField::Content => self.average_content_len,
            LexicalField::Tags => self.average_tag_len,
            LexicalField::Concepts => self.average_concept_len,
            LexicalField::AgentId => self.average_agent_id_len,
            LexicalField::AgentRegistry => self.average_agent_registry_len,
        }
    }

    /// Return postings for one single normalized term.
    ///
    /// If `term` normalizes to zero or multiple tokens, this returns `None`.
    pub fn postings(&self, term: &str) -> Option<&[LexicalPosting]> {
        let normalized = normalized_single_term(term)?;
        self.postings.get(&normalized).map(Vec::as_slice)
    }

    /// Return the document frequency for one normalized term.
    pub fn document_frequency(&self, term: &str) -> usize {
        self.postings(term).map_or(0, |postings| postings.len())
    }

    /// Run lexical ranking across the entire index.
    pub fn search(&self, query: &LexicalQuery) -> Vec<LexicalHit> {
        self.search_candidates(query, LexicalCandidates::All)
    }

    /// Run lexical ranking over a caller-provided candidate set.
    ///
    /// Passing an empty `candidate_positions` slice searches the full index
    /// (historical behavior kept for compatibility). Callers that need an
    /// empty candidate set to match nothing should use
    /// [`LexicalIndex::search_candidates`] with [`LexicalCandidates::Subset`].
    pub fn search_in_positions(
        &self,
        query: &LexicalQuery,
        candidate_positions: &[usize],
    ) -> Vec<LexicalHit> {
        let candidates = if candidate_positions.is_empty() {
            LexicalCandidates::All
        } else {
            LexicalCandidates::Subset(candidate_positions)
        };
        self.search_candidates(query, candidates)
    }

    /// Run lexical ranking over an explicit [`LexicalCandidates`] restriction.
    ///
    /// Hits are ordered by score, then importance, confidence, recency and
    /// thought index. When `query.limit` is set only the head is sorted and
    /// materialized.
    pub fn search_candidates(
        &self,
        query: &LexicalQuery,
        candidates: LexicalCandidates<'_>,
    ) -> Vec<LexicalHit> {
        let scan = self.scan(query, candidates);
        let mut order: Vec<usize> = scan.touched.clone();
        let compare = |left: &usize, right: &usize| {
            let left_stats = &self.document_stats[*left];
            let right_stats = &self.document_stats[*right];
            scan.scores[*right]
                .total_cmp(&scan.scores[*left])
                .then_with(|| right_stats.importance.total_cmp(&left_stats.importance))
                .then_with(|| {
                    right_stats
                        .confidence
                        .unwrap_or_default()
                        .total_cmp(&left_stats.confidence.unwrap_or_default())
                })
                .then_with(|| right_stats.timestamp.cmp(&left_stats.timestamp))
                .then_with(|| right_stats.thought_index.cmp(&left_stats.thought_index))
        };
        if let Some(limit) = query.limit {
            if limit == 0 {
                return Vec::new();
            }
            if order.len() > limit {
                order.select_nth_unstable_by(limit - 1, compare);
                order.truncate(limit);
            }
        }
        order.sort_by(compare);
        order
            .into_iter()
            .map(|position| self.hit_from_scan(&scan, position))
            .collect()
    }

    /// Score the query against the index and return the dense, unordered
    /// [`LexicalScan`].
    ///
    /// This is the allocation-light primitive behind
    /// [`LexicalIndex::search_candidates`]: scores accumulate in a dense
    /// `Vec<f32>` indexed by document position and matched terms/sources are
    /// recorded as bitmasks, so no per-hit strings are built until
    /// [`LexicalIndex::hit_from_scan`] is called for a document that is
    /// actually returned. An empty [`LexicalCandidates::Subset`] returns an
    /// empty scan without touching any posting list.
    pub fn scan(&self, query: &LexicalQuery, candidates: LexicalCandidates<'_>) -> LexicalScan {
        let doc_total = self.document_stats.len();
        let expanded = query.expanded_terms();
        let empty = |terms: Vec<(String, f32)>| LexicalScan {
            scores: Vec::new(),
            touched: Vec::new(),
            terms: terms.into_iter().map(|(term, _)| term).collect(),
            words_per_doc: 1,
            term_masks: Vec::new(),
            source_masks: Vec::new(),
        };
        if expanded.is_empty() || doc_total == 0 {
            return empty(expanded);
        }
        let allowed: Option<Vec<bool>> = match candidates {
            LexicalCandidates::All => None,
            LexicalCandidates::Subset(positions) => {
                if positions.is_empty() {
                    return empty(expanded);
                }
                let mut mask = vec![false; doc_total];
                for &position in positions {
                    if let Some(slot) = mask.get_mut(position) {
                        *slot = true;
                    }
                }
                Some(mask)
            }
        };

        let words_per_doc = expanded.len().div_ceil(64);
        let doc_count = doc_total as f32;
        let mut scores = vec![0.0_f32; doc_total];
        let mut touched = Vec::new();
        let mut term_masks = vec![0_u64; doc_total * words_per_doc];
        let mut source_masks = vec![0_u8; doc_total];
        let scoring = &query.scoring;
        let cutoffs = &query.df_cutoffs;
        let averages = [
            self.average_content_len,
            self.average_tag_len,
            self.average_concept_len,
            self.average_agent_id_len,
            self.average_agent_registry_len,
        ];
        let weights = [
            scoring.content_weight,
            scoring.tag_weight,
            scoring.concept_weight,
            scoring.agent_id_weight,
            scoring.agent_registry_weight,
        ];
        let cutoff_ratios = [
            cutoffs.content,
            cutoffs.tags,
            cutoffs.concepts,
            cutoffs.agent_id,
            cutoffs.agent_registry,
        ];

        for (term_index, (term, term_weight)) in expanded.iter().enumerate() {
            let Some(postings) = self.postings.get(term) else {
                continue;
            };
            let global_df = postings.len() as f32;
            let global_df_ratio = global_df / doc_count;
            let field_allowed: [bool; 5] =
                std::array::from_fn(|i| doc_count < 20.0 || global_df_ratio <= cutoff_ratios[i]);
            if !field_allowed.iter().any(|allowed| *allowed) {
                continue;
            }
            let idf = bm25_idf(doc_count, global_df);
            let word = term_index / 64;
            let bit = 1_u64 << (term_index % 64);

            for posting in postings {
                let position = posting.doc_position;
                if allowed.as_ref().is_some_and(|mask| !mask[position]) {
                    continue;
                }
                let stats = &self.document_stats[position];
                let mut score = 0.0_f32;
                let mut sources = 0_u8;
                for (field_index, field) in LEXICAL_FIELDS.iter().enumerate() {
                    if !field_allowed[field_index] {
                        continue;
                    }
                    let field_score = bm25_field_score(
                        posting.term_frequency(*field),
                        stats.field_len(*field),
                        averages[field_index],
                        idf,
                        scoring.k1,
                        scoring.b,
                    ) * weights[field_index]
                        * term_weight;
                    if field_score > 0.0 {
                        score += field_score;
                        sources |= 1 << field_index;
                    }
                }
                if score > 0.0 {
                    if scores[position] == 0.0 {
                        touched.push(position);
                    }
                    scores[position] += score;
                    term_masks[position * words_per_doc + word] |= bit;
                    source_masks[position] |= sources;
                }
            }
        }

        LexicalScan {
            scores,
            touched,
            terms: expanded.into_iter().map(|(term, _)| term).collect(),
            words_per_doc,
            term_masks,
            source_masks,
        }
    }

    /// Materialize one [`LexicalHit`] for a document touched by `scan`.
    ///
    /// Matched terms are listed in query-term order and match sources in
    /// canonical field order (content, tags, concepts, agent id, registry).
    /// Positions not touched by the scan yield a zero-score hit with no terms.
    pub fn hit_from_scan(&self, scan: &LexicalScan, doc_position: usize) -> LexicalHit {
        let stats = &self.document_stats[doc_position];
        let words = scan
            .term_masks
            .get(doc_position * scan.words_per_doc..(doc_position + 1) * scan.words_per_doc)
            .unwrap_or(&[]);
        let matched_terms = scan
            .terms
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                words
                    .get(index / 64)
                    .is_some_and(|w| (w >> (index % 64)) & 1 == 1)
            })
            .map(|(_, term)| term.clone())
            .collect();
        let source_bits = scan.source_masks.get(doc_position).copied().unwrap_or(0);
        let match_sources = LEXICAL_FIELDS
            .iter()
            .enumerate()
            .filter(|(index, _)| (source_bits >> index) & 1 == 1)
            .map(|(_, field)| match_source_for_field(*field))
            .collect();
        LexicalHit {
            doc_position,
            thought_index: stats.thought_index,
            thought_id: stats.thought_id,
            score: scan.scores.get(doc_position).copied().unwrap_or(0.0),
            matched_terms,
            match_sources,
        }
    }
}

const LEXICAL_FIELDS: [LexicalField; 5] = [
    LexicalField::Content,
    LexicalField::Tags,
    LexicalField::Concepts,
    LexicalField::AgentId,
    LexicalField::AgentRegistry,
];

fn match_source_for_field(field: LexicalField) -> LexicalMatchSource {
    match field {
        LexicalField::Content => LexicalMatchSource::Content,
        LexicalField::Tags => LexicalMatchSource::Tags,
        LexicalField::Concepts => LexicalMatchSource::Concepts,
        LexicalField::AgentId => LexicalMatchSource::AgentId,
        LexicalField::AgentRegistry => LexicalMatchSource::AgentRegistry,
    }
}

/// Restriction of lexical scoring to a subset of indexed documents.
///
/// Unlike the legacy "empty slice means everything" convention of
/// [`LexicalIndex::search_in_positions`], an empty [`Subset`](Self::Subset)
/// matches nothing, so a filter that selects zero thoughts never pays for a
/// full-index scan.
#[derive(Debug, Clone, Copy)]
pub enum LexicalCandidates<'a> {
    /// Score every indexed document.
    All,
    /// Score only documents at these positions; an empty slice matches nothing.
    Subset(&'a [usize]),
}

/// Dense, unordered lexical scoring result produced by [`LexicalIndex::scan`].
///
/// Holds one `f32` score per indexed document (zero means "no match"), the
/// list of matching positions, and compact bitmasks recording which query
/// terms and fields matched each document.
#[derive(Debug, Clone)]
pub struct LexicalScan {
    scores: Vec<f32>,
    touched: Vec<usize>,
    terms: Vec<String>,
    words_per_doc: usize,
    term_masks: Vec<u64>,
    source_masks: Vec<u8>,
}

impl LexicalScan {
    /// Number of documents with a positive score.
    pub fn len(&self) -> usize {
        self.touched.len()
    }

    /// Return `true` when no document matched.
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }

    /// Return the positive score for a document position, if it matched.
    pub fn score(&self, doc_position: usize) -> Option<f32> {
        self.scores
            .get(doc_position)
            .copied()
            .filter(|score| *score > 0.0)
    }

    /// Positions of matching documents in first-touched order.
    pub fn positions(&self) -> &[usize] {
        &self.touched
    }

    /// Return the `limit` best `(position, score)` pairs, best first, with
    /// ties broken by ascending position.
    ///
    /// Uses a partial selection so the cost is `O(matches + limit log limit)`.
    pub fn top_scores(&self, limit: usize) -> Vec<(usize, f32)> {
        if limit == 0 {
            return Vec::new();
        }
        let mut entries: Vec<(usize, f32)> = self
            .touched
            .iter()
            .map(|&position| (position, self.scores[position]))
            .collect();
        let order = |a: &(usize, f32), b: &(usize, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
        if entries.len() > limit {
            entries.select_nth_unstable_by(limit - 1, order);
            entries.truncate(limit);
        }
        entries.sort_by(order);
        entries
    }
}

fn english_stemmer() -> &'static rust_stemmers::Stemmer {
    use rust_stemmers::{Algorithm, Stemmer};
    static STEMMER: std::sync::OnceLock<Stemmer> = std::sync::OnceLock::new();
    STEMMER.get_or_init(|| Stemmer::create(Algorithm::English))
}

/// Normalize free-form text into versioned lexical tokens.
///
/// Tokenization splits on non-alphanumeric boundaries, lowercases, and then
/// applies Porter stemming so that word variants share a common root
/// (e.g. "prefers", "preferred", "preferences" → "prefer").
///
/// If `expand_lemmas` is true (for query-time use), irregular verb forms
/// are also expanded — e.g. "went" → "go" and "ran" → "run" — because
/// Porter stemming cannot normalize irregular past tenses.
///
/// Set `expand_lemmas` to false during index building to avoid duplicate
/// postings for terms whose lemma also appears explicitly in the text.
pub fn normalize_lexical_tokens(text: &str, expand_lemmas: bool) -> Vec<String> {
    let stemmer = english_stemmer();
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            let stemmed = stemmer.stem(&current).to_string();
            let lemma_stemmed = if expand_lemmas {
                super::lemmas::expand_lemma(&current).and_then(|lemma| {
                    let ls = stemmer.stem(lemma).to_string();
                    if ls == stemmed {
                        None
                    } else {
                        Some(ls)
                    }
                })
            } else {
                None
            };
            tokens.push(stemmed);
            if let Some(ls) = lemma_stemmed {
                tokens.push(ls);
            }
            current.clear();
        }
    }

    if !current.is_empty() {
        let stemmed = stemmer.stem(&current).to_string();
        let lemma_stemmed = if expand_lemmas {
            super::lemmas::expand_lemma(&current).and_then(|lemma| {
                let ls = stemmer.stem(lemma).to_string();
                if ls == stemmed {
                    None
                } else {
                    Some(ls)
                }
            })
        } else {
            None
        };
        tokens.push(stemmed);
        if let Some(ls) = lemma_stemmed {
            tokens.push(ls);
        }
    }

    tokens
}

fn observe_tokens(
    doc_position: usize,
    tokens: &[String],
    field: LexicalField,
    frequencies: &mut HashMap<String, LexicalPosting>,
) {
    for token in tokens {
        let posting = frequencies
            .entry(token.clone())
            .or_insert_with(|| LexicalPosting {
                doc_position,
                content_term_frequency: 0,
                tag_term_frequency: 0,
                concept_term_frequency: 0,
                agent_id_term_frequency: 0,
                agent_registry_term_frequency: 0,
            });
        match field {
            LexicalField::Content => posting.content_term_frequency += 1,
            LexicalField::Tags => posting.tag_term_frequency += 1,
            LexicalField::Concepts => posting.concept_term_frequency += 1,
            LexicalField::AgentId => posting.agent_id_term_frequency += 1,
            LexicalField::AgentRegistry => posting.agent_registry_term_frequency += 1,
        }
    }
}

fn agent_registry_tokens(record: &crate::AgentRecord) -> Vec<String> {
    let mut text_parts = vec![record.display_name.clone()];
    if let Some(owner) = &record.owner {
        text_parts.push(owner.clone());
    }
    if let Some(description) = &record.description {
        text_parts.push(description.clone());
    }
    text_parts.extend(record.aliases.iter().cloned());
    normalize_lexical_tokens(&text_parts.join(" "), false)
}

fn unique_normalized_terms(text: &str) -> Vec<String> {
    use rust_stemmers::{Algorithm, Stemmer};

    let stemmer = Stemmer::create(Algorithm::English);
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    let mut push = |token: &str| {
        let stemmed = stemmer.stem(token).to_string();
        if seen.insert(stemmed.clone()) {
            result.push(stemmed);
        }
        if let Some(lemma) = super::lemmas::expand_lemma(token) {
            let lemma_stemmed = stemmer.stem(lemma).to_string();
            if seen.insert(lemma_stemmed.clone()) {
                result.push(lemma_stemmed);
            }
        }
    };

    let mut raw = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            raw.extend(ch.to_lowercase());
        } else if !raw.is_empty() {
            push(&raw);
            raw.clear();
        }
    }
    if !raw.is_empty() {
        push(&raw);
    }

    result
}

fn normalized_single_term(term: &str) -> Option<String> {
    let mut tokens = normalize_lexical_tokens(term, true).into_iter();
    let first = tokens.next()?;
    if tokens.next().is_some() {
        return None;
    }
    Some(first)
}

fn average_length(total_length: u64, document_count: usize) -> f32 {
    if document_count == 0 {
        0.0
    } else {
        total_length as f32 / document_count as f32
    }
}

fn bm25_idf(doc_count: f32, document_frequency: f32) -> f32 {
    (((doc_count - document_frequency + 0.5) / (document_frequency + 0.5)) + 1.0).ln()
}

fn bm25_field_score(
    term_frequency: u32,
    document_length: u32,
    average_length: f32,
    idf: f32,
    k1: f32,
    b: f32,
) -> f32 {
    if term_frequency == 0 {
        return 0.0;
    }

    let tf = term_frequency as f32;
    let normalized_average = average_length.max(1.0);
    let length_ratio = document_length as f32 / normalized_average;
    let denominator = tf + k1 * (1.0 - b + b * length_ratio);
    idf * (tf * (k1 + 1.0)) / denominator
}
