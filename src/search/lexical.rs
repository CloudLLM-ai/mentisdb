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
    /// Which document frequency the cutoffs are compared against.
    ///
    /// Defaults to [`DfScope::Global`].
    pub df_scope: DfScope,
    /// Optional synonym expansion map: each original term maps to a list of
    /// synonyms that should also be searched with reduced weight.
    pub synonyms: std::collections::HashMap<String, Vec<String>>,
    /// Weight applied to synonym matches relative to original term matches.
    pub synonym_weight: f32,
    /// When `true` and the scan is restricted to a candidate subset, the
    /// corpus statistics behind the score (document count, document
    /// frequency, field length averages) are computed over that subset only.
    ///
    /// Scores otherwise depend on the whole chain, so a reader who may not see
    /// part of it could learn from score differences how common a term is in
    /// the hidden part. Set by [`crate::MentisDb::query_ranked`] for a
    /// [`crate::audience::Audience::Public`] reader of a chain with private
    /// segments. Defaults to `false`.
    pub isolate_corpus: bool,
    /// Normalized (stemmed) synonym terms keyed by normalized query term,
    /// built once by [`LexicalQuery::with_synonyms`].
    normalized_synonyms: Option<HashMap<String, Vec<String>>>,
}

/// Maximum number of normalized synonym terms added per query term.
///
/// Caps term explosion for headwords with long synonym lists: every extra
/// term costs a posting-list scan, and a long tail of loosely related words
/// dilutes the exact-term signal (LongMemEval R@5 and LoCoMo R@10 both
/// improved when the cap went from 5 to 3).
pub const MAX_SYNONYMS_PER_TERM: usize = 3;

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
            df_scope: DfScope::default(),
            synonyms: std::collections::HashMap::new(),
            synonym_weight: 0.7,
            isolate_corpus: false,
            normalized_synonyms: None,
        }
    }

    /// Compute corpus statistics over the candidate subset only. See
    /// [`LexicalQuery::isolate_corpus`].
    pub fn with_isolated_corpus(mut self, isolate: bool) -> Self {
        self.isolate_corpus = isolate;
        self
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

    /// Choose which document frequency the cutoffs are compared against.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::search::lexical::{DfScope, LexicalQuery};
    ///
    /// let query = LexicalQuery::new("rollback").with_df_scope(DfScope::PerField);
    /// assert_eq!(query.df_scope, DfScope::PerField);
    /// ```
    pub fn with_df_scope(mut self, scope: DfScope) -> Self {
        self.df_scope = scope;
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

/// How a term's document frequency is measured when it is checked against a
/// field's cutoff in [`Bm25DfCutoffs`].
///
/// The default, [`DfScope::Global`], is what shipped through 0.10.10 and what
/// every published benchmark number was produced with. [`DfScope::PerField`]
/// counts each field separately (`WHITEPAPER.md` section 6.2). It is opt-in because
/// it changes which terms score and therefore needs a LoCoMo/LongMemEval run
/// before it can become the default.
///
/// # Examples
///
/// ```rust
/// use mentisdb::search::lexical::DfScope;
///
/// assert_eq!(DfScope::default(), DfScope::Global);
/// assert_eq!("field".parse::<DfScope>().unwrap(), DfScope::PerField);
/// assert_eq!(DfScope::PerField.to_string(), "field");
/// assert!("bogus".parse::<DfScope>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DfScope {
    /// One document frequency per term: the number of thoughts that contain the
    /// term in **any** indexed field (content, tags, concepts, agent id, agent
    /// registry text). Every field's cutoff is compared against that single
    /// ratio.
    ///
    /// Side effect: agent id and agent registry text are attached to every
    /// thought of an agent, so a word that appears in an agent's display name,
    /// description or owner (a project or person name, "developer", "owner")
    /// counts as present in all of that agent's thoughts. Such a word can be
    /// gated out of `content` even though it occurs in only a few percent of
    /// the chain's content.
    #[default]
    Global,
    /// One document frequency per field: the number of thoughts whose *own*
    /// field contains the term. A word that is rare in content stays scoring in
    /// content no matter how many agents mention it in their registry text.
    PerField,
}

/// Error returned when a string is not a valid [`DfScope`] name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDfScopeError(pub String);

impl std::fmt::Display for ParseDfScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown DF scope {:?} (expected \"global\" or \"field\")",
            self.0
        )
    }
}

impl std::error::Error for ParseDfScopeError {}

impl std::fmt::Display for DfScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Global => "global",
            Self::PerField => "field",
        })
    }
}

impl std::str::FromStr for DfScope {
    type Err = ParseDfScopeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "global" => Ok(Self::Global),
            "field" | "per-field" | "per_field" | "perfield" => Ok(Self::PerField),
            other => Err(ParseDfScopeError(other.to_owned())),
        }
    }
}

impl DfScope {
    /// Name of the environment variable read by [`DfScope::from_env`].
    pub const ENV_VAR: &'static str = "MENTISDB_DF_SCOPE";

    /// Read `MENTISDB_DF_SCOPE` (`global` or `field`) once per process.
    ///
    /// An unset, empty or unrecognised value yields [`DfScope::Global`], the
    /// historical behaviour. This is what [`RankedSearchQuery`](crate::RankedSearchQuery)
    /// uses as its default, so a benchmark harness can compare both scopes
    /// without code changes.
    pub fn from_env() -> Self {
        static SCOPE: std::sync::OnceLock<DfScope> = std::sync::OnceLock::new();
        *SCOPE.get_or_init(|| {
            std::env::var(Self::ENV_VAR)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_default()
        })
    }
}

/// Per-field BM25 document-frequency cutoffs: the hard "stop-word" gate.
///
/// For every query term and every field, the term is **skipped for that field**
/// when `document_frequency / document_count > cutoff` (strictly greater than).
/// A skipped term contributes nothing from that field; it can still score
/// through a field whose cutoff it respects. The gate is a hard cliff, not a
/// down-weight: a term at 29% of the chain scores at full IDF-weighted strength
/// in `content`, and at 31% it scores zero there.
///
/// Further rules, all in [`LexicalIndex::scan`]:
///
/// * Gating is disabled entirely for chains of fewer than 20 thoughts, where a
///   percentage of the chain is meaningless.
/// * `document_count` is every committed thought of the chain, including
///   invalidated ones and including thoughts excluded by the query's filters.
/// * The IDF that weights a surviving term always uses the global document
///   frequency, whatever the [`DfScope`].
/// * Synonym terms are gated exactly like original terms.
/// * Which document frequency is compared against the cutoff depends on
///   [`DfScope`]: the default [`DfScope::Global`] counts a thought if the term
///   appears in *any* field, [`DfScope::PerField`] counts it per field.
///
/// MentisDB has no stop-word list; this gate is the only thing that silences
/// "the", "with" or "from". It also silences any real term that appears in more
/// than 30% of a chain, which in a single-project chain can include the
/// project's own vocabulary.
///
/// # Examples
///
/// ```rust
/// use mentisdb::search::lexical::{Bm25DfCutoffs, LexicalField};
///
/// let cutoffs = Bm25DfCutoffs::default();
/// assert_eq!(cutoffs.for_field(LexicalField::Content), 0.30);
/// assert_eq!(cutoffs.for_field(LexicalField::AgentId), 0.70);
/// ```
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
        let words_per_doc = expanded.len().div_ceil(64);
        let mut scratch = ScanScratch::acquire(doc_total, words_per_doc);
        let restrict = match candidates {
            LexicalCandidates::All => None,
            LexicalCandidates::Subset(positions) => {
                if positions.is_empty() {
                    scratch.release();
                    return empty(expanded);
                }
                if scratch.allowed.len() < doc_total {
                    scratch.allowed.resize(doc_total, false);
                }
                for &position in positions {
                    if let Some(slot) = scratch.allowed.get_mut(position) {
                        *slot = true;
                    }
                }
                Some(positions)
            }
        };

        // Isolated corpus: statistics of the allowed subset alone.
        let isolated = query.isolate_corpus && restrict.is_some();
        let mut isolated_averages = [0.0_f32; 5];
        let mut isolated_total = 0usize;
        if isolated {
            let mut sums = [0u64; 5];
            for (position, &allowed_flag) in scratch.allowed.iter().enumerate() {
                if !allowed_flag {
                    continue;
                }
                let Some(stats) = self.document_stats.get(position) else {
                    continue;
                };
                isolated_total += 1;
                for (i, field) in LEXICAL_FIELDS.iter().enumerate() {
                    sums[i] += u64::from(stats.field_len(*field));
                }
            }
            if isolated_total > 0 {
                for (average, sum) in isolated_averages.iter_mut().zip(sums) {
                    *average = sum as f32 / isolated_total as f32;
                }
            }
        }
        let doc_count = if isolated {
            isolated_total as f32
        } else {
            doc_total as f32
        };
        let mut touched = Vec::new();
        let ScanScratch {
            scores,
            term_masks,
            source_masks,
            allowed,
        } = &mut scratch;
        let scoring = &query.scoring;
        let cutoffs = &query.df_cutoffs;
        let averages = if isolated {
            isolated_averages
        } else {
            [
                self.average_content_len,
                self.average_tag_len,
                self.average_concept_len,
                self.average_agent_id_len,
                self.average_agent_registry_len,
            ]
        };
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
            let visible_postings =
                |posting: &&LexicalPosting| !isolated || allowed[posting.doc_position];
            let global_df = if isolated {
                postings.iter().filter(visible_postings).count() as f32
            } else {
                postings.len() as f32
            };
            if global_df == 0.0 {
                continue;
            }
            let global_df_ratio = global_df / doc_count;
            let field_allowed: [bool; 5] = match query.df_scope {
                DfScope::Global => {
                    std::array::from_fn(|i| doc_count < 20.0 || global_df_ratio <= cutoff_ratios[i])
                }
                DfScope::PerField => {
                    let mut field_df = [0_u32; 5];
                    for posting in postings.iter().filter(visible_postings) {
                        for (i, field) in LEXICAL_FIELDS.iter().enumerate() {
                            field_df[i] += u32::from(posting.term_frequency(*field) > 0);
                        }
                    }
                    std::array::from_fn(|i| {
                        doc_count < 20.0 || field_df[i] as f32 / doc_count <= cutoff_ratios[i]
                    })
                }
            };
            if !field_allowed.iter().any(|allowed| *allowed) {
                continue;
            }
            let idf = bm25_idf(doc_count, global_df);
            let word = term_index / 64;
            let bit = 1_u64 << (term_index % 64);

            for posting in postings {
                let position = posting.doc_position;
                if restrict.is_some() && !allowed[position] {
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

        // The restriction mask must be all-false again before the scratch is
        // shared; scores and masks are cleared when the scan is dropped.
        if let Some(positions) = restrict {
            for &position in positions {
                if let Some(slot) = allowed.get_mut(position) {
                    *slot = false;
                }
            }
        }
        let scan = LexicalScan {
            scores: std::mem::take(&mut scratch.scores),
            touched,
            terms: expanded.into_iter().map(|(term, _)| term).collect(),
            words_per_doc,
            term_masks: std::mem::take(&mut scratch.term_masks),
            source_masks: std::mem::take(&mut scratch.source_masks),
        };
        scratch.release();
        scan
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

/// Dense per-document scratch space reused across scans on one thread.
///
/// A scan needs `O(documents)` zeroed score, term-mask and source-mask arrays.
/// Allocating them per query made a one-term search on a large chain pay for
/// the whole index. Instead the arrays are kept, all-zero, in a per-thread
/// slot: a scan borrows them, and [`LexicalScan`]'s `Drop` clears only the
/// positions it touched before handing them back, so steady-state cost is
/// `O(matches)` rather than `O(documents)`.
///
/// Invariant: every array in the pool is entirely zero (or `false`).
#[derive(Default)]
struct ScanScratch {
    scores: Vec<f32>,
    term_masks: Vec<u64>,
    source_masks: Vec<u8>,
    allowed: Vec<bool>,
}

/// Largest scratch (in bytes) a thread keeps between scans; a giant one-off
/// index is released instead of pinning memory on every worker thread.
const SCAN_SCRATCH_RETAIN_BYTES: usize = 64 * 1024 * 1024;

thread_local! {
    static SCAN_SCRATCH: std::cell::RefCell<Option<ScanScratch>> =
        const { std::cell::RefCell::new(None) };
}

impl ScanScratch {
    /// Take the thread's scratch (or a fresh one) sized for `doc_total`
    /// documents and `words_per_doc` term-mask words per document.
    fn acquire(doc_total: usize, words_per_doc: usize) -> Self {
        let mut scratch = SCAN_SCRATCH
            .try_with(|slot| slot.try_borrow_mut().ok().and_then(|mut slot| slot.take()))
            .ok()
            .flatten()
            .unwrap_or_default();
        if scratch.scores.len() < doc_total {
            scratch.scores.resize(doc_total, 0.0);
            scratch.source_masks.resize(doc_total, 0);
        }
        let mask_words = doc_total * words_per_doc;
        if scratch.term_masks.len() < mask_words {
            scratch.term_masks.resize(mask_words, 0);
        }
        scratch
    }

    fn bytes(&self) -> usize {
        self.scores.len() * 4
            + self.term_masks.len() * 8
            + self.source_masks.len()
            + self.allowed.len()
    }

    /// Return an all-zero scratch to the thread's slot, keeping the larger
    /// array of each kind when the slot is already occupied (a scan's arrays
    /// and its restriction mask come back separately).
    fn release(self) {
        let _ = SCAN_SCRATCH.try_with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return;
            };
            let mut merged = slot.take().unwrap_or_default();
            merged.scores = longer(merged.scores, self.scores);
            merged.term_masks = longer(merged.term_masks, self.term_masks);
            merged.source_masks = longer(merged.source_masks, self.source_masks);
            merged.allowed = longer(merged.allowed, self.allowed);
            if merged.bytes() <= SCAN_SCRATCH_RETAIN_BYTES {
                *slot = Some(merged);
            }
        });
    }
}

/// The longer of two all-zero arrays.
fn longer<T>(left: Vec<T>, right: Vec<T>) -> Vec<T> {
    if right.len() > left.len() {
        right
    } else {
        left
    }
}

impl Drop for LexicalScan {
    fn drop(&mut self) {
        if self.scores.is_empty() {
            return;
        }
        let words = self.words_per_doc;
        for &position in &self.touched {
            self.scores[position] = 0.0;
            self.source_masks[position] = 0;
            self.term_masks[position * words..(position + 1) * words].fill(0);
        }
        ScanScratch {
            scores: std::mem::take(&mut self.scores),
            term_masks: std::mem::take(&mut self.term_masks),
            source_masks: std::mem::take(&mut self.source_masks),
            allowed: Vec::new(),
        }
        .release();
    }
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

/// Hash of the text an agent record contributes to the lexical index.
///
/// Used to notice that a registry edit (a description, owner, display name or
/// alias added after thoughts were written) changed what the index should hold
/// for every earlier thought of that agent.
pub(crate) fn agent_registry_text_hash(record: &crate::AgentRecord) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    agent_registry_tokens(record).hash(&mut hasher);
    hasher.finish()
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
