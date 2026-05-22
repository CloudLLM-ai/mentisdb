//! Pseudo-relevance feedback query expansion primitives.
//!
//! This module is intentionally isolated from ranked search wiring. Callers pass
//! either feedback documents or posting-like candidate statistics and receive a
//! deterministic expansion plan plus route metadata.

use super::lexical::normalize_lexical_tokens;
use std::collections::{HashMap, HashSet};

/// Configuration for pseudo-relevance feedback query expansion.
#[derive(Debug, Clone, PartialEq)]
pub struct PrfConfig {
    /// Whether PRF expansion is enabled.
    pub enabled: bool,
    /// Number of top feedback documents to inspect.
    pub feedback_docs: usize,
    /// Maximum number of expansion terms to return.
    pub expansion_terms: usize,
    /// Minimum BM25-style IDF required for a candidate term.
    pub min_idf: f32,
    /// Weight assigned to original query terms in route metadata.
    pub original_weight: f32,
    /// Weight assigned to expansion terms in route metadata.
    pub expansion_weight: f32,
}

impl Default for PrfConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            feedback_docs: 5,
            expansion_terms: 8,
            min_idf: 1.0,
            original_weight: 1.0,
            expansion_weight: 0.35,
        }
    }
}

/// One top-ranked document used as pseudo-relevance feedback.
#[derive(Debug, Clone, PartialEq)]
pub struct PrfFeedbackDocument {
    /// Zero-based rank from the original lexical route.
    pub rank: usize,
    /// Original lexical score. Non-positive scores are treated as noisy feedback.
    pub score: f32,
    /// Text to tokenize for candidate extraction.
    pub text: String,
}

/// Posting-like statistics for one candidate expansion term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrfTermCandidate {
    /// Normalized or raw candidate term. Multi-token values are ignored.
    pub term: String,
    /// Number of feedback documents containing this term.
    pub feedback_document_frequency: usize,
    /// Total candidate frequency across feedback documents.
    pub feedback_term_frequency: u32,
    /// Document frequency for this term in the full searchable collection.
    pub collection_document_frequency: usize,
    /// Best zero-based feedback rank containing this term.
    pub best_feedback_rank: usize,
}

/// One selected expansion term with deterministic scoring metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct PrfExpansionTerm {
    /// Normalized expansion term.
    pub term: String,
    /// BM25-style IDF computed from collection document frequency.
    pub idf: f32,
    /// Rocchio-like candidate weight before route-level fusion.
    pub weight: f32,
    /// Number of feedback documents containing this term.
    pub feedback_document_frequency: usize,
    /// Total candidate frequency across feedback documents.
    pub feedback_term_frequency: u32,
}

/// Route metadata that ranked search can attach to expanded lexical results.
#[derive(Debug, Clone, PartialEq)]
pub struct PrfRouteMetadata {
    /// Stable route label for ranked-search integration.
    pub route: &'static str,
    /// Original query terms preserved for scoring/fusion explanations.
    pub original_terms: Vec<String>,
    /// Weight assigned to original query terms.
    pub original_weight: f32,
    /// Weight assigned to PRF expansion terms.
    pub expansion_weight: f32,
    /// Number of feedback documents considered.
    pub feedback_docs_used: usize,
}

/// Result of attempting PRF query expansion.
#[derive(Debug, Clone, PartialEq)]
pub struct PrfExpansion {
    /// Normalized original query terms.
    pub original_terms: Vec<String>,
    /// Selected expansion terms.
    pub expansion_terms: Vec<PrfExpansionTerm>,
    /// Query text formed from original and expansion terms.
    pub expanded_query: String,
    /// Ranked-search route metadata.
    pub route_metadata: PrfRouteMetadata,
}

/// Expand a query from top feedback documents.
pub fn expand_query_from_feedback_docs(
    original_query: &str,
    feedback_docs: &[PrfFeedbackDocument],
    collection_document_count: usize,
    config: &PrfConfig,
) -> PrfExpansion {
    let original_terms = original_query_terms(original_query);
    if !config.enabled
        || config.feedback_docs == 0
        || config.expansion_terms == 0
        || feedback_docs.is_empty()
        || collection_document_count == 0
    {
        return no_op_expansion(original_terms, config, 0);
    }

    let used_docs = feedback_docs
        .iter()
        .filter(|doc| doc.score > 0.0)
        .take(config.feedback_docs)
        .collect::<Vec<_>>();

    if used_docs.is_empty() {
        return no_op_expansion(original_terms, config, 0);
    }

    let mut term_stats = HashMap::<String, PrfTermCandidate>::new();
    for doc in &used_docs {
        let feedback_rank = doc.rank;
        let tokens = normalize_lexical_tokens(&doc.text, false);
        let mut seen_in_doc = HashSet::new();
        for token in tokens {
            let entry = term_stats
                .entry(token.clone())
                .or_insert_with(|| PrfTermCandidate {
                    term: token.clone(),
                    feedback_document_frequency: 0,
                    feedback_term_frequency: 0,
                    collection_document_frequency: 0,
                    best_feedback_rank: feedback_rank,
                });
            entry.feedback_term_frequency += 1;
            entry.best_feedback_rank = entry.best_feedback_rank.min(feedback_rank);
            if seen_in_doc.insert(token) {
                entry.feedback_document_frequency += 1;
            }
        }
    }

    for candidate in term_stats.values_mut() {
        candidate.collection_document_frequency = candidate.feedback_document_frequency;
    }

    expand_query_from_candidates(
        original_query,
        term_stats.values().cloned().collect::<Vec<_>>().as_slice(),
        collection_document_count,
        config,
        used_docs.len(),
    )
}

/// Expand a query from posting-like candidate statistics.
pub fn expand_query_from_candidates(
    original_query: &str,
    candidates: &[PrfTermCandidate],
    collection_document_count: usize,
    config: &PrfConfig,
    feedback_docs_used: usize,
) -> PrfExpansion {
    let original_terms = original_query_terms(original_query);
    if !config.enabled
        || config.expansion_terms == 0
        || feedback_docs_used == 0
        || collection_document_count == 0
        || candidates.is_empty()
    {
        return no_op_expansion(original_terms, config, feedback_docs_used);
    }

    let expansion_terms = choose_expansion_terms(
        &original_terms,
        candidates,
        collection_document_count,
        config,
    );
    let expanded_query = expanded_query_text(&original_terms, &expansion_terms);
    PrfExpansion {
        route_metadata: PrfRouteMetadata {
            route: "prf_expanded_lexical",
            original_terms: original_terms.clone(),
            original_weight: config.original_weight,
            expansion_weight: config.expansion_weight,
            feedback_docs_used,
        },
        original_terms,
        expansion_terms,
        expanded_query,
    }
}

/// Choose expansion terms deterministically from posting-like candidates.
pub fn choose_expansion_terms(
    original_terms: &[String],
    candidates: &[PrfTermCandidate],
    collection_document_count: usize,
    config: &PrfConfig,
) -> Vec<PrfExpansionTerm> {
    if !config.enabled || config.expansion_terms == 0 || collection_document_count == 0 {
        return Vec::new();
    }

    let original = original_terms.iter().cloned().collect::<HashSet<_>>();
    let mut selected = Vec::new();
    for candidate in candidates {
        let Some(term) = normalized_candidate_term(&candidate.term) else {
            continue;
        };
        if original.contains(&term)
            || is_stopword(&term)
            || candidate.feedback_document_frequency == 0
            || candidate.feedback_term_frequency == 0
        {
            continue;
        }

        let df = candidate
            .collection_document_frequency
            .max(candidate.feedback_document_frequency);
        let idf = bm25_idf(collection_document_count, df);
        if idf < config.min_idf {
            continue;
        }

        let tf_weight = 1.0 + (candidate.feedback_term_frequency as f32).ln();
        let feedback_coverage = candidate.feedback_document_frequency as f32;
        let rank_discount = 1.0 / (candidate.best_feedback_rank as f32 + 1.0);
        selected.push(PrfExpansionTerm {
            term,
            idf,
            weight: config.expansion_weight * idf * tf_weight * feedback_coverage * rank_discount,
            feedback_document_frequency: candidate.feedback_document_frequency,
            feedback_term_frequency: candidate.feedback_term_frequency,
        });
    }

    selected.sort_by(|left, right| {
        right
            .weight
            .total_cmp(&left.weight)
            .then_with(|| right.idf.total_cmp(&left.idf))
            .then_with(|| {
                right
                    .feedback_term_frequency
                    .cmp(&left.feedback_term_frequency)
            })
            .then_with(|| left.term.cmp(&right.term))
    });
    selected.dedup_by(|left, right| left.term == right.term);
    selected.truncate(config.expansion_terms);
    selected
}

fn original_query_terms(original_query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for term in normalize_lexical_tokens(original_query, true) {
        if !is_stopword(&term) && seen.insert(term.clone()) {
            terms.push(term);
        }
    }
    terms
}

fn normalized_candidate_term(term: &str) -> Option<String> {
    let mut terms = normalize_lexical_tokens(term, false).into_iter();
    let first = terms.next()?;
    if terms.next().is_some() {
        None
    } else {
        Some(first)
    }
}

fn no_op_expansion(
    original_terms: Vec<String>,
    config: &PrfConfig,
    feedback_docs_used: usize,
) -> PrfExpansion {
    PrfExpansion {
        expanded_query: original_terms.join(" "),
        route_metadata: PrfRouteMetadata {
            route: "prf_expanded_lexical",
            original_terms: original_terms.clone(),
            original_weight: config.original_weight,
            expansion_weight: config.expansion_weight,
            feedback_docs_used,
        },
        original_terms,
        expansion_terms: Vec::new(),
    }
}

fn expanded_query_text(original_terms: &[String], expansion_terms: &[PrfExpansionTerm]) -> String {
    original_terms
        .iter()
        .chain(expansion_terms.iter().map(|term| &term.term))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

fn bm25_idf(collection_document_count: usize, document_frequency: usize) -> f32 {
    let n = collection_document_count as f32;
    let df = document_frequency.min(collection_document_count) as f32;
    (((n - df + 0.5) / (df + 0.5)) + 1.0).ln()
}

fn is_stopword(term: &str) -> bool {
    matches!(
        term,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "for"
            | "from"
            | "ha"
            | "have"
            | "he"
            | "her"
            | "hi"
            | "i"
            | "in"
            | "is"
            | "it"
            | "its"
            | "of"
            | "on"
            | "or"
            | "our"
            | "she"
            | "that"
            | "the"
            | "their"
            | "them"
            | "thi"
            | "to"
            | "wa"
            | "we"
            | "were"
            | "with"
            | "you"
            | "your"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> PrfConfig {
        PrfConfig {
            enabled: true,
            feedback_docs: 3,
            expansion_terms: 3,
            min_idf: 0.5,
            original_weight: 1.0,
            expansion_weight: 0.4,
        }
    }

    #[test]
    fn expands_vocabulary_mismatch_from_feedback_docs() {
        let feedback_docs = vec![PrfFeedbackDocument {
            rank: 0,
            score: 2.0,
            text: "invoice vendor payment invoice vendor".to_string(),
        }];

        let expansion =
            expand_query_from_feedback_docs("trip cost", &feedback_docs, 40, &enabled_config());

        let terms = expansion
            .expansion_terms
            .iter()
            .map(|term| term.term.as_str())
            .collect::<Vec<_>>();
        assert_eq!(terms, vec!["invoic", "vendor", "payment"]);
        assert_eq!(expansion.original_terms, vec!["trip", "cost"]);
        assert_eq!(expansion.route_metadata.route, "prf_expanded_lexical");
        assert_eq!(expansion.route_metadata.feedback_docs_used, 1);
    }

    #[test]
    fn filters_common_stopword_and_original_query_terms() {
        let config = enabled_config();
        let candidates = vec![
            PrfTermCandidate {
                term: "trip".to_string(),
                feedback_document_frequency: 2,
                feedback_term_frequency: 5,
                collection_document_frequency: 2,
                best_feedback_rank: 0,
            },
            PrfTermCandidate {
                term: "the".to_string(),
                feedback_document_frequency: 2,
                feedback_term_frequency: 10,
                collection_document_frequency: 2,
                best_feedback_rank: 0,
            },
            PrfTermCandidate {
                term: "common".to_string(),
                feedback_document_frequency: 2,
                feedback_term_frequency: 8,
                collection_document_frequency: 95,
                best_feedback_rank: 0,
            },
            PrfTermCandidate {
                term: "invoice".to_string(),
                feedback_document_frequency: 1,
                feedback_term_frequency: 2,
                collection_document_frequency: 3,
                best_feedback_rank: 0,
            },
        ];

        let expansion = expand_query_from_candidates("trip cost", &candidates, 100, &config, 2);

        assert_eq!(expansion.expansion_terms.len(), 1);
        assert_eq!(expansion.expansion_terms[0].term, "invoic");
    }

    #[test]
    fn disabled_and_no_feedback_are_no_ops() {
        let mut config = enabled_config();
        config.enabled = false;
        let candidates = vec![PrfTermCandidate {
            term: "invoice".to_string(),
            feedback_document_frequency: 1,
            feedback_term_frequency: 2,
            collection_document_frequency: 1,
            best_feedback_rank: 0,
        }];

        let disabled = expand_query_from_candidates("trip cost", &candidates, 10, &config, 1);
        assert!(disabled.expansion_terms.is_empty());
        assert_eq!(disabled.expanded_query, "trip cost");

        config.enabled = true;
        let no_feedback = expand_query_from_candidates("trip cost", &candidates, 10, &config, 0);
        assert!(no_feedback.expansion_terms.is_empty());
        assert_eq!(no_feedback.expanded_query, "trip cost");
    }
}
