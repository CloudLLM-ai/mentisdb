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
pub struct LexicalQuery {
    /// Raw query text that will be normalized using the current normalizer.
    pub text: String,
    /// Optional maximum number of ranked hits to return.
    pub limit: Option<usize>,
    /// BM25-style scoring configuration.
    pub scoring: LexicalScoringConfig,
    /// Per-field document-frequency cutoffs for stop-word suppression.
    pub df_cutoffs: Bm25DfCutoffs,
}

impl LexicalQuery {
    /// Create a new lexical query with default scoring settings.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: None,
            scoring: LexicalScoringConfig::default(),
            df_cutoffs: Bm25DfCutoffs::default(),
        }
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
}

/// Per-field BM25 document-frequency cutoffs.
///
/// Terms whose document frequency exceeds `cutoff_ratio * N` in a given
/// field are skipped for that field only. This prevents stop words from
/// dominating content scoring while still allowing high-frequency tag or
/// concept matches to contribute.
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
            tags: 0.50,
            concepts: 0.40,
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
            let content_tokens = normalize_lexical_tokens(&thought.content);
            let tag_tokens = thought
                .tags
                .iter()
                .flat_map(|tag| normalize_lexical_tokens(tag))
                .collect::<Vec<_>>();
            let concept_tokens = thought
                .concepts
                .iter()
                .flat_map(|concept| normalize_lexical_tokens(concept))
                .collect::<Vec<_>>();
            let agent_id_tokens = normalize_lexical_tokens(&thought.agent_id);
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
        self.search_in_positions(query, &[])
    }

    /// Run lexical ranking over a caller-provided candidate set.
    ///
    /// Passing an empty `candidate_positions` slice searches the full index.
    pub fn search_in_positions(
        &self,
        query: &LexicalQuery,
        candidate_positions: &[usize],
    ) -> Vec<LexicalHit> {
        let terms = query.normalized_terms();
        if terms.is_empty() || self.document_stats.is_empty() {
            return Vec::new();
        }

        let candidate_filter = if candidate_positions.is_empty() {
            None
        } else {
            Some(
                candidate_positions
                    .iter()
                    .copied()
                    .collect::<HashSet<usize>>(),
            )
        };

        let doc_count = self.document_stats.len() as f32;
        let mut scores = HashMap::<usize, f32>::new();
        let mut matched_terms = HashMap::<usize, Vec<String>>::new();
        let mut match_sources = HashMap::<usize, Vec<LexicalMatchSource>>::new();

        for term in terms {
            let Some(postings) = self.postings.get(&term) else {
                continue;
            };
            let content_df = postings
                .iter()
                .filter(|p| p.content_term_frequency > 0)
                .count() as f32;
            let tag_df = postings.iter().filter(|p| p.tag_term_frequency > 0).count() as f32;
            let concept_df = postings
                .iter()
                .filter(|p| p.concept_term_frequency > 0)
                .count() as f32;
            let agent_id_df = postings
                .iter()
                .filter(|p| p.agent_id_term_frequency > 0)
                .count() as f32;
            let agent_registry_df = postings
                .iter()
                .filter(|p| p.agent_registry_term_frequency > 0)
                .count() as f32;
            let content_allowed =
                doc_count < 20.0 || content_df / doc_count <= query.df_cutoffs.content;
            let tags_allowed = doc_count < 20.0 || tag_df / doc_count <= query.df_cutoffs.tags;
            let concepts_allowed =
                doc_count < 20.0 || concept_df / doc_count <= query.df_cutoffs.concepts;
            let agent_id_allowed =
                doc_count < 20.0 || agent_id_df / doc_count <= query.df_cutoffs.agent_id;
            let agent_registry_allowed = doc_count < 20.0
                || agent_registry_df / doc_count <= query.df_cutoffs.agent_registry;
            if !content_allowed
                && !tags_allowed
                && !concepts_allowed
                && !agent_id_allowed
                && !agent_registry_allowed
            {
                continue;
            }
            let term_df = postings.len() as f32;
            let idf = bm25_idf(doc_count, term_df);

            for posting in postings {
                if candidate_filter
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&posting.doc_position))
                {
                    continue;
                }
                let stats = &self.document_stats[posting.doc_position];
                let content_score = if content_allowed {
                    bm25_field_score(
                        posting.content_term_frequency,
                        stats.content_len,
                        self.average_content_len,
                        idf,
                        query.scoring.k1,
                        query.scoring.b,
                    ) * query.scoring.content_weight
                } else {
                    0.0
                };
                let tag_score = if tags_allowed {
                    bm25_field_score(
                        posting.tag_term_frequency,
                        stats.tag_len,
                        self.average_tag_len,
                        idf,
                        query.scoring.k1,
                        query.scoring.b,
                    ) * query.scoring.tag_weight
                } else {
                    0.0
                };
                let concept_score = if concepts_allowed {
                    bm25_field_score(
                        posting.concept_term_frequency,
                        stats.concept_len,
                        self.average_concept_len,
                        idf,
                        query.scoring.k1,
                        query.scoring.b,
                    ) * query.scoring.concept_weight
                } else {
                    0.0
                };
                let agent_id_score = if agent_id_allowed {
                    bm25_field_score(
                        posting.agent_id_term_frequency,
                        stats.agent_id_len,
                        self.average_agent_id_len,
                        idf,
                        query.scoring.k1,
                        query.scoring.b,
                    ) * query.scoring.agent_id_weight
                } else {
                    0.0
                };
                let agent_registry_score = if agent_registry_allowed {
                    bm25_field_score(
                        posting.agent_registry_term_frequency,
                        stats.agent_registry_len,
                        self.average_agent_registry_len,
                        idf,
                        query.scoring.k1,
                        query.scoring.b,
                    ) * query.scoring.agent_registry_weight
                } else {
                    0.0
                };
                let score = content_score
                    + tag_score
                    + concept_score
                    + agent_id_score
                    + agent_registry_score;

                if score > 0.0 {
                    *scores.entry(posting.doc_position).or_insert(0.0) += score;
                    push_unique_string(
                        matched_terms.entry(posting.doc_position).or_default(),
                        &term,
                    );
                    let sources = match_sources.entry(posting.doc_position).or_default();
                    if content_score > 0.0 {
                        push_unique_match_source(sources, LexicalMatchSource::Content);
                    }
                    if tag_score > 0.0 {
                        push_unique_match_source(sources, LexicalMatchSource::Tags);
                    }
                    if concept_score > 0.0 {
                        push_unique_match_source(sources, LexicalMatchSource::Concepts);
                    }
                    if agent_id_score > 0.0 {
                        push_unique_match_source(sources, LexicalMatchSource::AgentId);
                    }
                    if agent_registry_score > 0.0 {
                        push_unique_match_source(sources, LexicalMatchSource::AgentRegistry);
                    }
                }
            }
        }

        let mut hits = scores
            .into_iter()
            .map(|(doc_position, score)| {
                let stats = &self.document_stats[doc_position];
                LexicalHit {
                    doc_position,
                    thought_index: stats.thought_index,
                    thought_id: stats.thought_id,
                    score,
                    matched_terms: matched_terms.remove(&doc_position).unwrap_or_default(),
                    match_sources: match_sources.remove(&doc_position).unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>();

        hits.sort_by(|left, right| {
            let left_stats = &self.document_stats[left.doc_position];
            let right_stats = &self.document_stats[right.doc_position];
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right_stats.importance.total_cmp(&left_stats.importance))
                .then_with(|| {
                    right_stats
                        .confidence
                        .unwrap_or_default()
                        .total_cmp(&left_stats.confidence.unwrap_or_default())
                })
                .then_with(|| right_stats.timestamp.cmp(&left_stats.timestamp))
                .then_with(|| right_stats.thought_index.cmp(&left_stats.thought_index))
        });

        if let Some(limit) = query.limit {
            hits.truncate(limit);
        }
        hits
    }
}

/// Normalize free-form text into versioned lexical tokens.
///
/// Tokenization splits on non-alphanumeric boundaries, lowercases, and then
/// applies Porter stemming so that word variants share a common root
/// (e.g. "prefers", "preferred", "preferences" → "prefer").
pub fn normalize_lexical_tokens(text: &str) -> Vec<String> {
    use rust_stemmers::{Algorithm, Stemmer};

    let stemmer = Stemmer::create(Algorithm::English);
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            let stemmed = stemmer.stem(&current).to_string();
            tokens.push(stemmed);
            current.clear();
        }
    }

    if !current.is_empty() {
        let stemmed = stemmer.stem(&current).to_string();
        tokens.push(stemmed);
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

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn push_unique_match_source(values: &mut Vec<LexicalMatchSource>, value: LexicalMatchSource) {
    if !values.contains(&value) {
        values.push(value);
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
    normalize_lexical_tokens(&text_parts.join(" "))
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
    let mut tokens = normalize_lexical_tokens(term).into_iter();
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
