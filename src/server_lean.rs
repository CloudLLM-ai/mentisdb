//! Lean MCP output: what an agent's context window pays for a tool call.
//!
//! The streamable-HTTP MCP surface (JSON-RPC `tools/call`) is read by language
//! models, so every character of a tool result is a token the agent spends.
//! REST responses are read by programs and keep their full, stable shape. This
//! module reshapes tool results **only** on the MCP surface, in four steps.
//!
//! # 1. Minified text, one copy
//!
//! The MCP library turns an object result into pretty-printed JSON text *and*
//! repeats the same object as `structuredContent`. Measured on a 3,500-thought
//! chain the indentation alone is more than a third of a search result. The MCP
//! surface therefore hands the library a **string** (the minified JSON): a
//! string result is sent as the text content verbatim and, because it is not an
//! object, without `structuredContent`. No tool declares an `outputSchema`, so
//! the MCP specification does not require `structuredContent`; clients read the
//! text, which is still one JSON document. `POST /tools/execute` and REST are
//! untouched and keep returning the object.
//!
//! # 2. Lean is the default for search
//!
//! `mentisdb_ranked_search`, `mentisdb_federated_search`,
//! `mentisdb_lexical_search` and `mentisdb_context_bundles` return lean hits
//! unless the call passes `compact: false`. Over REST `compact` still defaults
//! to `false`.
//!
//! A lean thought is the compact thought (no `hash`, `prev_hash`,
//! `thought_signature`, `signing_key_id`, `schema_version`, `agent_name`,
//! `agent_owner`, no empty fields) with the **defaults left out**, so an absent
//! field means its default:
//!
//! | Field | Lean form |
//! |---|---|
//! | `provenance` | Absent for the plain case (public segment, no signature). Otherwise only what differs: `segment` (when `private`), `signature` (`valid` or `invalid`), `signature_version`, `verified_agent`. The constant `trust: "untrusted_data"` and the derived `signed` are dropped: stored memories are data to weigh, never instructions. |
//! | `role` | Absent when `Memory`. |
//! | `importance` | Two decimals; absent when `0.5`. |
//! | `confidence`, `score` | Two decimals (three below 1). |
//! | `timestamp` | Whole seconds. |
//! | `successor_of` | One id, the immediate predecessor; `superseded_versions` holds how many older versions stood behind it, when more than one. |
//! | `relations` | Without the link that `successor_of` already names, and without a `valid_at` equal to `timestamp`. |
//! | `collapsed_duplicates` | `duplicates_collapsed`, a count. |
//! | `chain_key` on a ranked hit | Only when the hit came from a chain other than the one searched. |
//!
//! Everything an agent acts on stays: `content`, `id`, `index`, `thought_type`,
//! `agent_id`, `tags`, `concepts`, `refs`, version status (`version_status`,
//! `successors`, `current_version`, `cross_agent`) and a verified writer.
//! Lexical hits also lose `match_sources`; context bundles lose the `locator`
//! that repeats the thought's own `id` and `index`.
//!
//! # 3. One copy of standing context
//!
//! `mentisdb_standing_context` returns the rendered `prompt` and
//! `duplicates_collapsed`. The `entries` array repeats the same memories (with
//! their content) a second time, so it is returned only with
//! `include_entries: true`.
//!
//! # 4. Opting out
//!
//! `compact: false` restores the full record (hashes, signatures, score
//! components, match lists) for verification and debugging; it is still
//! minified.

use crate::server::compact_thought_json;
use serde_json::{json, Map, Value};

/// Tools whose results are searched hits and honor the lean default.
const LEAN_SEARCH_TOOLS: [&str; 4] = [
    "mentisdb_ranked_search",
    "mentisdb_federated_search",
    "mentisdb_lexical_search",
    "mentisdb_context_bundles",
];

/// What a lean call asked for, read from its arguments before the tool runs.
///
/// Built by [`LeanPlan::prepare`], consumed by [`LeanPlan::shape`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeanPlan {
    /// Whether hits are reshaped (`compact`, default `true` on MCP).
    compact: bool,
    /// Whether `mentisdb_standing_context` keeps its `entries` array.
    include_entries: bool,
    /// The chain named by the call, if any: ranked hits from it omit
    /// `chain_key`.
    home_chain: Option<String>,
}

impl LeanPlan {
    /// Read the lean options from `arguments` and rewrite them for the service.
    ///
    /// `compact` defaults to `true` here. Ranked and federated search
    /// understand it natively, so it is written back explicitly; lexical search
    /// and context bundles do not, so it is removed. `include_entries` is
    /// removed too. Other tools pass through untouched.
    pub(crate) fn prepare(tool: &str, arguments: &mut Value) -> Self {
        let Some(map) = arguments.as_object_mut() else {
            return Self::plain();
        };
        let include_entries = map
            .remove("include_entries")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let compact = LEAN_SEARCH_TOOLS.contains(&tool)
            && map.get("compact").and_then(Value::as_bool).unwrap_or(true);
        if matches!(tool, "mentisdb_ranked_search" | "mentisdb_federated_search") {
            map.insert("compact".into(), Value::Bool(compact));
        } else {
            map.remove("compact");
        }
        Self {
            compact,
            include_entries,
            home_chain: map
                .get("chain_key")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }

    fn plain() -> Self {
        Self {
            compact: false,
            include_entries: false,
            home_chain: None,
        }
    }

    /// Reshape a tool's `output` and serialize it as minified JSON text.
    ///
    /// A string output (already text) is returned as is. See the module
    /// documentation for what changes per tool.
    pub(crate) fn shape(&self, tool: &str, output: Value) -> Value {
        let shaped = match tool {
            "mentisdb_ranked_search" | "mentisdb_federated_search" if self.compact => {
                self.lean_ranked(output, tool == "mentisdb_ranked_search")
            }
            "mentisdb_lexical_search" if self.compact => lean_lexical(output),
            "mentisdb_context_bundles" if self.compact => lean_bundles(output),
            "mentisdb_standing_context" if !self.include_entries => drop_key(output, "entries"),
            _ => output,
        };
        match shaped {
            Value::String(_) => shaped,
            other => Value::String(other.to_string()),
        }
    }

    fn lean_ranked(&self, mut output: Value, single_chain: bool) -> Value {
        let Some(hits) = output.get_mut("results").and_then(Value::as_array_mut) else {
            return output;
        };
        // A ranked search may also read ancestor chains: a hit only names its
        // chain when it is not the one the agent searched.
        let home = single_chain
            .then(|| self.home_chain.clone().or_else(|| common_chain_key(hits)))
            .flatten();
        for hit in hits {
            let Some(fields) = hit.as_object_mut() else {
                continue;
            };
            if home.is_some() && fields.get("chain_key").and_then(Value::as_str) == home.as_deref()
            {
                fields.remove("chain_key");
            }
            round_field(fields, "score");
            if let Some(thought) = fields.get_mut("thought") {
                *thought = lean_thought(std::mem::take(thought));
            }
        }
        output
    }
}

/// The chain key every hit shares, if they all carry the same one.
fn common_chain_key(hits: &[Value]) -> Option<String> {
    let first = hits.first()?.get("chain_key")?.as_str()?;
    hits.iter()
        .all(|hit| hit.get("chain_key").and_then(Value::as_str) == Some(first))
        .then(|| first.to_owned())
}

fn drop_key(mut output: Value, key: &str) -> Value {
    if let Some(map) = output.as_object_mut() {
        map.remove(key);
    }
    output
}

fn lean_lexical(mut output: Value) -> Value {
    let Some(results) = output.get_mut("results").and_then(Value::as_array_mut) else {
        return output;
    };
    for result in results {
        let Some(fields) = result.as_object_mut() else {
            continue;
        };
        fields.remove("match_sources");
        round_field(fields, "score");
        if let Some(thought) = fields.get_mut("thought") {
            *thought = lean_thought(compact_thought_json(std::mem::take(thought)));
        }
    }
    output
}

fn lean_bundles(mut output: Value) -> Value {
    let Some(bundles) = output.get_mut("bundles").and_then(Value::as_array_mut) else {
        return output;
    };
    for bundle in bundles {
        if let Some(seed) = bundle.get_mut("seed").and_then(Value::as_object_mut) {
            round_field(seed, "lexical_score");
            lean_bundle_thought(seed);
        }
        let support = bundle.get_mut("support").and_then(Value::as_array_mut);
        for item in support.into_iter().flatten() {
            if let Some(fields) = item.as_object_mut() {
                lean_bundle_thought(fields);
            }
        }
    }
    output
}

/// Lean the `thought` of a bundle seed or support hit and drop the `locator`
/// that only repeats the thought's own `id` and `index`.
fn lean_bundle_thought(fields: &mut Map<String, Value>) {
    let Some(thought) = fields.get_mut("thought").filter(|t| t.is_object()) else {
        return;
    };
    *thought = lean_thought(compact_thought_json(std::mem::take(thought)));
    fields.remove("locator");
}

/// Round a numeric field to two decimals (three below 1), in place.
fn round_field(fields: &mut Map<String, Value>, key: &str) {
    if let Some(rounded) = fields.get(key).and_then(Value::as_f64).map(round_score) {
        fields.insert(key.to_owned(), rounded);
    }
}

/// `x` rounded to 2 decimals, or 3 when `|x| < 1`, as a JSON number.
fn round_score(x: f64) -> Value {
    let scale = if x.abs() < 1.0 { 1000.0 } else { 100.0 };
    json!((x * scale).round() / scale)
}

/// Whether a provenance object is the plain public, unsigned case.
fn provenance_is_plain(p: &Map<String, Value>) -> bool {
    p.get("segment").and_then(Value::as_str).unwrap_or("public") == "public"
        && p.get("signature")
            .and_then(Value::as_str)
            .unwrap_or("unsigned")
            == "unsigned"
        && p.get("verified_agent").is_none_or(Value::is_null)
        && p.get("signature_version").is_none_or(Value::is_null)
}

/// The provenance fields that differ from the plain case.
fn lean_provenance(p: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, plain) in [("segment", "public"), ("signature", "unsigned")] {
        if let Some(v) = p.get(key).filter(|v| v.as_str() != Some(plain)) {
            out.insert(key.to_owned(), v.clone());
        }
    }
    for key in ["signature_version", "verified_agent"] {
        if let Some(v) = p.get(key).filter(|v| !v.is_null()) {
            out.insert(key.to_owned(), v.clone());
        }
    }
    out
}

/// `2026-11-08T00:46:34.624687Z` as `2026-11-08T00:46:34Z`; other shapes are
/// returned unchanged.
fn whole_seconds(timestamp: &str) -> String {
    match (timestamp.find('.'), timestamp.strip_suffix('Z')) {
        (Some(dot), Some(_)) if dot < timestamp.len() => format!("{}Z", &timestamp[..dot]),
        _ => timestamp.to_owned(),
    }
}

/// Reshape one (already compact) thought into its lean form. See the module
/// documentation for the rules. A value that is not an object is returned
/// unchanged.
pub(crate) fn lean_thought(thought: Value) -> Value {
    let Value::Object(mut fields) = thought else {
        return thought;
    };
    lean_provenance_field(&mut fields);
    if fields.get("role").and_then(Value::as_str) == Some("Memory") {
        fields.remove("role");
    }
    if fields.get("importance").and_then(Value::as_f64) == Some(0.5) {
        fields.remove("importance");
    } else {
        round_field(&mut fields, "importance");
    }
    round_field(&mut fields, "confidence");
    let timestamp = fields
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_owned);
    lean_lineage(&mut fields, timestamp.as_deref());
    if let Some(ts) = timestamp {
        fields.insert("timestamp".into(), Value::String(whole_seconds(&ts)));
    }
    if let Some(Value::Array(ids)) = fields.remove("collapsed_duplicates") {
        fields.insert("duplicates_collapsed".into(), json!(ids.len()));
    }
    Value::Object(fields)
}

fn lean_provenance_field(fields: &mut Map<String, Value>) {
    let Some(Value::Object(provenance)) = fields.get("provenance") else {
        return;
    };
    if provenance_is_plain(provenance) {
        fields.remove("provenance");
    } else {
        let lean = lean_provenance(provenance);
        fields.insert("provenance".into(), Value::Object(lean));
    }
}

/// Collapse `successor_of` to the immediate predecessor and drop what it makes
/// redundant in `relations`.
fn lean_lineage(fields: &mut Map<String, Value>, timestamp: Option<&str>) {
    let predecessors: Vec<Value> = match fields.get("successor_of") {
        Some(Value::Array(ids)) => ids.clone(),
        _ => Vec::new(),
    };
    let immediate = immediate_predecessor(fields, &predecessors);
    if let Some(id) = &immediate {
        fields.insert("successor_of".into(), json!([id]));
        if predecessors.len() > 1 {
            fields.insert("superseded_versions".into(), json!(predecessors.len()));
        }
    }
    if let Some(Value::Array(relations)) = fields.get_mut("relations") {
        relations.retain_mut(|relation| {
            let Some(fields) = relation.as_object_mut() else {
                return true;
            };
            let target = fields.get("target_id").and_then(Value::as_str);
            let strong = matches!(
                fields.get("kind").and_then(Value::as_str),
                Some("Supersedes" | "Corrects" | "Invalidates")
            );
            if strong && target.is_some() && target == immediate.as_deref() {
                return false;
            }
            if fields.get("valid_at").and_then(Value::as_str) == timestamp {
                fields.remove("valid_at");
            }
            true
        });
    }
    if fields
        .get("relations")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        fields.remove("relations");
    }
}

/// The predecessor this thought directly replaces: the target of its own
/// strong relation when that is among `predecessors`, else the first one.
fn immediate_predecessor(fields: &Map<String, Value>, predecessors: &[Value]) -> Option<String> {
    let listed = |id: &str| predecessors.iter().any(|p| p.as_str() == Some(id));
    let linked = fields
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| {
            matches!(
                r.get("kind").and_then(Value::as_str),
                Some("Supersedes" | "Corrects" | "Invalidates")
            )
        })
        .filter_map(|r| r.get("target_id").and_then(Value::as_str))
        .find(|id| listed(id));
    linked
        .or_else(|| predecessors.first().and_then(Value::as_str))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_provenance() -> Value {
        json!({"segment": "public", "signature": "unsigned", "signature_version": null,
               "signed": false, "trust": "untrusted_data", "verified_agent": null})
    }

    /// The plain public, unsigned provenance disappears; defaults go with it.
    #[test]
    fn test_lean_thought_elides_plain_provenance_and_defaults() {
        let lean = lean_thought(json!({
            "content": "x", "id": "a", "index": 3, "role": "Memory", "importance": 0.5,
            "provenance": plain_provenance(),
            "timestamp": "2026-11-08T00:46:34.624687Z",
        }));
        assert_eq!(
            lean,
            json!({"content": "x", "id": "a", "index": 3, "timestamp": "2026-11-08T00:46:34Z"})
        );
    }

    /// A private, signed or verified provenance keeps only what differs, and a
    /// verified writer stays visible.
    #[test]
    fn test_lean_thought_keeps_what_differs_in_provenance() {
        let lean = lean_thought(json!({
            "content": "x",
            "provenance": {"segment": "private", "signed": true, "signature": "valid",
                           "signature_version": 2, "verified_agent": "codex",
                           "trust": "untrusted_data"},
        }));
        assert_eq!(
            lean["provenance"],
            json!({"segment": "private", "signature": "valid", "signature_version": 2,
                   "verified_agent": "codex"})
        );
        let invalid = lean_thought(json!({"provenance": {"segment": "public", "signed": true,
            "signature": "invalid", "signature_version": null, "verified_agent": null,
            "trust": "untrusted_data"}}));
        assert_eq!(invalid["provenance"], json!({"signature": "invalid"}));
    }

    /// Float noise from `f32` is rounded; non-default values stay.
    #[test]
    fn test_lean_thought_rounds_floats() {
        let lean = lean_thought(json!({"importance": 0.800000011920929,
                                       "confidence": 0.7000000476837158}));
        assert_eq!(lean, json!({"importance": 0.8, "confidence": 0.7}));
        assert_eq!(round_score(31.61272621154785), json!(31.61));
        assert_eq!(round_score(0.034567), json!(0.035));
    }

    /// `successor_of` keeps the immediate predecessor and a count; the relation
    /// that names it is dropped, an unrelated one stays.
    #[test]
    fn test_lean_thought_collapses_successor_of() {
        let lean = lean_thought(json!({
            "successor_of": ["old1", "old2", "old3"],
            "relations": [
                {"kind": "Supersedes", "target_id": "old2", "valid_at": "2026-01-01T00:00:00.5Z"},
                {"kind": "References", "target_id": "other", "valid_at": "2026-01-01T00:00:00.5Z"}
            ],
            "timestamp": "2026-01-01T00:00:00.5Z",
        }));
        assert_eq!(lean["successor_of"], json!(["old2"]));
        assert_eq!(lean["superseded_versions"], json!(3));
        assert_eq!(
            lean["relations"],
            json!([{"kind": "References", "target_id": "other"}])
        );
    }

    /// A lone predecessor needs no count, and without a linking relation the
    /// first listed id is kept.
    #[test]
    fn test_lean_thought_single_predecessor_has_no_count() {
        let lean = lean_thought(json!({"successor_of": ["old"], "relations": []}));
        assert_eq!(lean, json!({"successor_of": ["old"]}));
    }

    /// Duplicate ids collapse to a count.
    #[test]
    fn test_lean_thought_counts_collapsed_duplicates() {
        let lean = lean_thought(json!({"collapsed_duplicates": ["a", "b"]}));
        assert_eq!(lean, json!({"duplicates_collapsed": 2}));
    }

    /// A value that is not an object passes through.
    #[test]
    fn test_lean_thought_ignores_non_objects() {
        assert_eq!(lean_thought(json!(null)), json!(null));
        assert_eq!(lean_thought(json!("x")), json!("x"));
    }

    /// `compact` defaults to true for searches, is forwarded to the services
    /// that know it, removed for the ones that do not, and can be turned off.
    #[test]
    fn test_prepare_defaults_compact_for_search_tools() {
        let mut args = json!({"text": "q"});
        let plan = LeanPlan::prepare("mentisdb_ranked_search", &mut args);
        assert!(plan.compact);
        assert_eq!(args["compact"], json!(true));

        let mut args = json!({"text": "q", "compact": false});
        let plan = LeanPlan::prepare("mentisdb_federated_search", &mut args);
        assert!(!plan.compact);
        assert_eq!(args["compact"], json!(false));

        let mut args = json!({"text": "q", "compact": true});
        let plan = LeanPlan::prepare("mentisdb_lexical_search", &mut args);
        assert!(plan.compact);
        assert!(args.get("compact").is_none());

        let mut args = json!({"chain_key": "c"});
        let plan = LeanPlan::prepare("mentisdb_head", &mut args);
        assert!(!plan.compact);
        assert_eq!(args, json!({"chain_key": "c"}));
    }

    /// Standing context drops `entries` unless asked, and the result is one
    /// minified string.
    #[test]
    fn test_shape_standing_context_drops_entries_by_default() {
        let output =
            json!({"entries": [{"content": "x"}], "prompt": "p", "duplicates_collapsed": 0});
        let mut args = json!({});
        let plan = LeanPlan::prepare("mentisdb_standing_context", &mut args);
        let text = plan.shape("mentisdb_standing_context", output.clone());
        assert_eq!(text, json!(r#"{"duplicates_collapsed":0,"prompt":"p"}"#));

        let mut args = json!({"include_entries": true});
        let plan = LeanPlan::prepare("mentisdb_standing_context", &mut args);
        assert!(args.get("include_entries").is_none());
        let text = plan.shape("mentisdb_standing_context", output);
        assert!(text.as_str().unwrap().contains("\"entries\""));
    }

    /// A ranked hit from the searched chain loses `chain_key`; one from another
    /// chain keeps it.
    #[test]
    fn test_ranked_hits_name_only_foreign_chains() {
        let mut args = json!({"chain_key": "home"});
        let plan = LeanPlan::prepare("mentisdb_ranked_search", &mut args);
        let output = json!({"total": 2, "results": [
            {"chain_key": "home", "score": 24.917037963867188, "thought": {"content": "a"}},
            {"chain_key": "parent", "score": 3.0, "thought": {"content": "b"}}]});
        let shaped = plan.shape("mentisdb_ranked_search", output);
        let parsed: Value = serde_json::from_str(shaped.as_str().unwrap()).unwrap();
        assert!(parsed["results"][0].get("chain_key").is_none());
        assert_eq!(parsed["results"][0]["score"], json!(24.92));
        assert_eq!(parsed["results"][1]["chain_key"], json!("parent"));
    }

    /// With no chain named, hits that all share one chain drop the key, but
    /// federated hits always keep it.
    #[test]
    fn test_chain_key_elision_without_explicit_chain() {
        let output = json!({"results": [
            {"chain_key": "d", "score": 1.0, "thought": {}},
            {"chain_key": "d", "score": 1.0, "thought": {}}]});
        let mut args = json!({});
        let plan = LeanPlan::prepare("mentisdb_ranked_search", &mut args);
        let shaped = plan.shape("mentisdb_ranked_search", output.clone());
        assert!(!shaped.as_str().unwrap().contains("chain_key"));
        let mut args = json!({});
        let plan = LeanPlan::prepare("mentisdb_federated_search", &mut args);
        let shaped = plan.shape("mentisdb_federated_search", output);
        assert!(shaped.as_str().unwrap().contains("chain_key"));
    }
}
