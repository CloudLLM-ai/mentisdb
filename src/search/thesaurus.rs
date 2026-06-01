//! Static thesaurus for query-time synonym expansion.
//!
//! Provides a comprehensive, deterministic lookup from query terms to
//! semantically related words. The thesaurus data is embedded into the
//! binary at compile time via `include_str!` — no external files or
//! network calls are required at runtime.
//!
//! Features:
//! - **~950 headwords** covering common English verbs, nouns, adjectives,
//!   adverbs, and technical terminology.
//! - **Unidirectional lookup**: synonyms are read directly from the embedded
//!   data file. Many terms appear as headwords in both directions (e.g. both
//!   "fast" and "quick" have entries), so practical coverage is extensive.
//! - **Verb form handling**: query terms are lemmatized (irregular + regular)
//!   before thesaurus lookup, so "went" finds "go" synonyms.
//! - **Deterministic**: identical queries always produce identical synonym maps.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Raw thesaurus data embedded into the binary.
static THESAURUS_DATA: &str = include_str!("thesaurus_data.txt");

/// Parsed unidirectional thesaurus map.
///
/// Initialized lazily on first access. Each key is a headword from the
/// embedded data file; its value is the explicit synonym list for that
/// headword. Lemmatization is applied at query time to bridge verb-form
/// gaps (e.g. "went" → "go").
static THESAURUS_MAP: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();

fn parse_thesaurus() -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for line in THESAURUS_DATA.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };

        let key = key.trim().to_string();
        let synonyms: Vec<String> = rest
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != &key)
            .collect();

        if synonyms.is_empty() {
            continue;
        }

        let entry = map.entry(key).or_default();
        for s in synonyms {
            if !entry.contains(&s) {
                entry.push(s);
            }
        }
    }

    map
}

/// Return a reference to the parsed bidirectional thesaurus map.
fn thesaurus_map() -> &'static HashMap<String, Vec<String>> {
    THESAURUS_MAP.get_or_init(parse_thesaurus)
}

/// Lookup synonyms for a single term from the built-in thesaurus.
///
/// Returns `None` if the term has no thesaurus entry.
pub fn lookup(term: &str) -> Option<Vec<String>> {
    thesaurus_map().get(term).cloned()
}

/// Lemmatize a token using irregular + regular verb rules, then look it up
/// in the thesaurus.
///
/// This bridges verb form gaps (e.g. "went" → "go" → synonyms).
pub fn lookup_lemmatized(token: &str) -> Option<Vec<String>> {
    // Try the raw token first.
    if let Some(syms) = lookup(token) {
        return Some(syms);
    }

    // Try lemmatization (irregular verbs + regular suffix stripping).
    if let Some(base) = crate::search::lemmas::lemmatize(token) {
        if let Some(syms) = lookup(&base) {
            return Some(syms);
        }
    }

    None
}

/// Expand a query text into a synonym map using the built-in thesaurus.
///
/// Each key is a normalized query term. The value is the list of synonyms
/// (with duplicates removed). The function tries both the raw token and
/// its lemmatized form, so past-tense and perfect-tense verbs are handled
/// automatically.
///
/// # Example
/// ```
/// let map = mentisdb::search::thesaurus::expand_text("fast search debugging");
/// assert!(map.contains_key("fast"));
/// assert!(map["fast"].iter().any(|term| term == "quick"));
/// assert!(map["search"].iter().any(|term| term == "query"));
/// assert!(map["debugging"].iter().any(|term| term == "diagnose"));
/// ```
pub fn expand_text(text: &str) -> HashMap<String, Vec<String>> {
    let mut result = HashMap::new();

    for raw in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        let term = raw.to_ascii_lowercase();
        if let Some(syms) = lookup_lemmatized(&term) {
            result.entry(term).or_insert_with(|| syms);
        }
    }

    result
}

/// Return the number of headwords in the thesaurus.
pub fn headword_count() -> usize {
    thesaurus_map().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thesaurus_loaded_and_non_empty() {
        let map = thesaurus_map();
        assert!(!map.is_empty(), "thesaurus should not be empty");
        assert!(
            map.len() >= 850,
            "thesaurus should have 850+ headwords, got {}",
            map.len()
        );
    }

    #[test]
    fn lookup_finds_fast_synonyms() {
        let syms = lookup("fast").expect("'fast' should be in thesaurus");
        assert!(
            syms.contains(&"quick".to_string()),
            "fast should map to quick"
        );
        assert!(
            syms.contains(&"rapid".to_string()),
            "fast should map to rapid"
        );
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("xyz_unknown_term").is_none());
    }

    #[test]
    fn expand_text_extracts_relevant_terms() {
        let expanded = expand_text("fast search debugging");
        assert!(expanded.contains_key("fast"), "should contain 'fast'");
        assert!(expanded.contains_key("search"), "should contain 'search'");
        assert!(
            expanded.contains_key("debugging"),
            "should contain 'debugging' via lemmatization"
        );
        assert!(
            !expanded.contains_key("unknown"),
            "should not contain unknown"
        );
    }

    #[test]
    fn lemmatized_lookup_finds_went() {
        // "went" is not in thesaurus directly, but "go" is.
        let syms = lookup_lemmatized("went").expect("'went' should lemmatize to 'go'");
        assert!(
            syms.contains(&"travel".to_string()),
            "went/go should map to travel"
        );
    }

    #[test]
    fn lemmatized_lookup_finds_walked() {
        // "walked" is not in thesaurus, but "walk" is.
        let syms = lookup_lemmatized("walked").expect("'walked' should lemmatize to 'walk'");
        assert!(
            syms.contains(&"step".to_string()),
            "walked/walk should map to step"
        );
    }

    #[test]
    fn headword_count_is_reasonable() {
        let count = headword_count();
        assert!(count >= 850, "expected 850+ headwords, got {}", count);
    }
}
