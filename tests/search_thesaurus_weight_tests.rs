//! Regression tests for the automatic thesaurus weight and per-term cap.
//!
//! Stemmed synonym keys (d0bad01) made expansion fire for inflected words and
//! at weight 0.7 cost ~2.4 pt of LongMemEval R@5. These tests pin the damped
//! defaults and the relative ordering they guarantee.

use mentisdb::search::lexical::{LexicalQuery, MAX_SYNONYMS_PER_TERM};
use mentisdb::search::thesaurus::{self, AUTO_SYNONYM_WEIGHT};

/// The automatic weight must stay a minor signal next to an exact-term match.
#[test]
fn auto_synonym_weight_is_well_below_exact_term_weight() {
    let weight = std::hint::black_box(AUTO_SYNONYM_WEIGHT);
    assert!(weight > 0.0);
    assert!(weight <= 0.35, "got {weight}");
}

/// Expansion of an inflected word still works but is capped and damped.
#[test]
fn inflected_expansion_uses_auto_weight_and_cap() {
    let map = thesaurus::expand_text("running");
    assert!(!map.is_empty(), "inflected term should still expand");
    let query = LexicalQuery::new("running").with_synonyms(map, AUTO_SYNONYM_WEIGHT);
    let terms = query.expanded_terms();
    let (exact, synonyms): (Vec<_>, Vec<_>) = terms.iter().partition(|(_, w)| *w == 1.0);
    assert_eq!(exact.len(), 1);
    assert!(!synonyms.is_empty());
    assert!(synonyms.len() <= MAX_SYNONYMS_PER_TERM);
    assert!(synonyms.iter().all(|(_, w)| *w == AUTO_SYNONYM_WEIGHT));
}
