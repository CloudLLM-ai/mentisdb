//! Deterministic query intent classification and retrieval-route weighting.
//!
//! This module is intentionally self-contained: it does not call LLMs, read or
//! write chain state, or depend on persisted indexes. Integration code can use
//! it as a pure planning step before selecting retrieval routes.

/// Heuristic intent signals inferred from a user query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryIntent {
    /// Query asks for time, chronology, ordering, or date-bounded facts.
    pub temporal: bool,
    /// Query names or appears to target an entity, concept, tag, topic, or type.
    pub entity_focused: bool,
    /// Query asks about an author, speaker, agent, or producer of a memory.
    pub agent_focused: bool,
    /// Query asks for cause, rationale, derivation, or consequence.
    pub causal: bool,
    /// Query is short or abstract enough to benefit from semantic matching.
    pub semantic: bool,
    /// Query asks for broad, roll-up, summary, or whole-chain context.
    pub summary_or_global: bool,
}

impl QueryIntent {
    /// Classify a query using deterministic token and phrase heuristics.
    ///
    /// `known_entities` should contain application-provided entity type names,
    /// concepts, tags, or other stable labels that the caller wants to route as
    /// entity-focused. Matching is case-insensitive and phrase-preserving.
    pub fn classify(query: &str, known_entities: &[&str]) -> Self {
        let normalized = normalize(query);
        let tokens = tokenize(&normalized);

        let temporal = contains_any(
            &tokens,
            &[
                "after",
                "ago",
                "before",
                "current",
                "date",
                "day",
                "during",
                "earlier",
                "hour",
                "latest",
                "later",
                "month",
                "recent",
                "recently",
                "since",
                "then",
                "time",
                "timeline",
                "today",
                "tomorrow",
                "until",
                "week",
                "when",
                "year",
                "yesterday",
            ],
        ) || contains_year_or_date(&tokens);

        let agent_focused = contains_any(
            &tokens,
            &[
                "agent", "author", "authors", "by", "from", "said", "speaker", "speakers", "who",
                "whom", "whose", "wrote",
            ],
        ) || contains_phrase(&normalized, "which agent");

        let causal = contains_any(
            &tokens,
            &[
                "because",
                "cause",
                "caused",
                "causes",
                "consequence",
                "derived",
                "due",
                "effect",
                "led",
                "reason",
                "reasons",
                "result",
                "triggered",
                "why",
            ],
        ) || contains_phrase(&normalized, "caused by")
            || contains_phrase(&normalized, "derived from")
            || contains_phrase(&normalized, "due to");

        let summary_or_global = contains_any(
            &tokens,
            &[
                "all",
                "everything",
                "global",
                "overall",
                "overview",
                "recap",
                "summaries",
                "summarize",
                "summary",
                "whole",
            ],
        ) || contains_phrase(&normalized, "all about")
            || contains_phrase(&normalized, "high level");

        let entity_focused = matches_known_entity(&normalized, known_entities)
            || contains_any(
                &tokens,
                &[
                    "about", "concept", "concepts", "entity", "entities", "project", "session",
                    "tag", "tags", "topic", "topics", "type",
                ],
            );

        let semantic = is_short_abstract_query(&tokens)
            || (!temporal && !agent_focused && !causal && !summary_or_global);

        Self {
            temporal,
            entity_focused,
            agent_focused,
            causal,
            semantic,
            summary_or_global,
        }
    }
}

/// Relative route weights selected from a [`QueryIntent`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryRoutingPlan {
    /// Weight for exact lexical/BM25-style matching.
    pub lexical_weight: f32,
    /// Weight for semantic vector matching.
    pub vector_weight: f32,
    /// Weight for explicit graph expansion.
    pub graph_weight: f32,
    /// Weight for personalized PageRank graph routing.
    pub ppr_weight: f32,
    /// Weight for temporal scoring or temporal reranking.
    pub temporal_weight: f32,
    /// Weight for summary hierarchy routing.
    pub summary_weight: f32,
    /// Whether pseudo-relevance feedback expansion should be attempted.
    pub enable_prf: bool,
}

impl QueryRoutingPlan {
    /// Return the legacy/default routing weights used when query routing is off.
    pub fn legacy_default() -> Self {
        Self {
            lexical_weight: 1.0,
            vector_weight: 1.0,
            graph_weight: 1.0,
            ppr_weight: 0.0,
            temporal_weight: 0.0,
            summary_weight: 0.0,
            enable_prf: false,
        }
    }

    /// Build route weights for an already-classified query intent.
    pub fn from_intent(intent: QueryIntent) -> Self {
        let mut plan = Self::legacy_default();

        if intent.semantic {
            plan.vector_weight = plan.vector_weight.max(1.25);
            plan.enable_prf = true;
        }

        if intent.entity_focused {
            plan.lexical_weight = plan.lexical_weight.max(1.25);
            plan.graph_weight = plan.graph_weight.max(1.1);
            plan.enable_prf = true;
        }

        if intent.agent_focused {
            plan.lexical_weight = plan.lexical_weight.max(1.3);
            plan.graph_weight = plan.graph_weight.max(1.1);
        }

        if intent.temporal {
            plan.lexical_weight = plan.lexical_weight.max(1.1);
            plan.vector_weight = plan.vector_weight.min(0.9);
            plan.temporal_weight = 1.5;
        }

        if intent.causal {
            plan.vector_weight = plan.vector_weight.max(1.1);
            plan.graph_weight = plan.graph_weight.max(1.4);
            plan.ppr_weight = plan.ppr_weight.max(1.2);
        }

        if intent.summary_or_global {
            plan.vector_weight = plan.vector_weight.max(1.2);
            plan.graph_weight = plan.graph_weight.max(1.1);
            plan.summary_weight = 1.5;
        }

        plan
    }

    /// Classify and plan a query, preserving legacy defaults when disabled.
    pub fn for_query(query: &str, known_entities: &[&str], enabled: bool) -> Self {
        if enabled {
            Self::from_intent(QueryIntent::classify(query, known_entities))
        } else {
            Self::legacy_default()
        }
    }
}

fn normalize(input: &str) -> String {
    input.to_ascii_lowercase()
}

fn tokenize(normalized: &str) -> Vec<&str> {
    normalized
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect()
}

fn contains_any(tokens: &[&str], needles: &[&str]) -> bool {
    tokens.iter().any(|token| needles.contains(token))
}

fn contains_phrase(normalized: &str, phrase: &str) -> bool {
    normalized.contains(phrase)
}

fn contains_year_or_date(tokens: &[&str]) -> bool {
    tokens.iter().any(|token| {
        (token.len() == 4 && token.starts_with('2') && token.chars().all(|ch| ch.is_ascii_digit()))
            || (token.len() >= 8 && token.chars().filter(|ch| ch.is_ascii_digit()).count() >= 6)
    })
}

fn matches_known_entity(normalized: &str, known_entities: &[&str]) -> bool {
    known_entities.iter().any(|entity| {
        let entity = entity.trim();
        !entity.is_empty() && normalized.contains(&entity.to_ascii_lowercase())
    })
}

fn is_short_abstract_query(tokens: &[&str]) -> bool {
    const ABSTRACT_TERMS: &[&str] = &[
        "decision",
        "decisions",
        "idea",
        "ideas",
        "insight",
        "insights",
        "lesson",
        "lessons",
        "memory",
        "memories",
        "preference",
        "preferences",
        "strategy",
        "theme",
        "themes",
    ];

    tokens.len() <= 4 && contains_any(tokens, ABSTRACT_TERMS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_did_query_boosts_temporal_route() {
        let intent = QueryIntent::classify("when did we decide to add PPR?", &[]);
        let plan = QueryRoutingPlan::from_intent(intent);

        assert!(intent.temporal);
        assert!(plan.temporal_weight > QueryRoutingPlan::legacy_default().temporal_weight);
        assert!(plan.lexical_weight >= 1.1);
    }

    #[test]
    fn who_said_query_focuses_agent_metadata() {
        let intent = QueryIntent::classify("who said the dashboard needed summaries?", &[]);
        let plan = QueryRoutingPlan::from_intent(intent);

        assert!(intent.agent_focused);
        assert!(plan.lexical_weight > QueryRoutingPlan::legacy_default().lexical_weight);
        assert!(plan.graph_weight >= QueryRoutingPlan::legacy_default().graph_weight);
    }

    #[test]
    fn why_did_query_boosts_causal_graph_routes() {
        let intent = QueryIntent::classify("why did retrieval quality regress?", &[]);
        let plan = QueryRoutingPlan::from_intent(intent);

        assert!(intent.causal);
        assert!(plan.graph_weight > QueryRoutingPlan::legacy_default().graph_weight);
        assert!(plan.ppr_weight > QueryRoutingPlan::legacy_default().ppr_weight);
    }

    #[test]
    fn summarize_query_routes_to_summaries() {
        let intent = QueryIntent::classify("summarize everything about query routing", &[]);
        let plan = QueryRoutingPlan::from_intent(intent);

        assert!(intent.summary_or_global);
        assert!(plan.summary_weight > QueryRoutingPlan::legacy_default().summary_weight);
    }

    #[test]
    fn known_entity_query_is_entity_focused() {
        let intent = QueryIntent::classify("find Acme billing decisions", &["Acme"]);
        let plan = QueryRoutingPlan::from_intent(intent);

        assert!(intent.entity_focused);
        assert!(plan.lexical_weight > QueryRoutingPlan::legacy_default().lexical_weight);
        assert!(plan.enable_prf);
    }

    #[test]
    fn disabled_routing_returns_legacy_defaults() {
        let plan = QueryRoutingPlan::for_query(
            "summarize why Acme changed plans in 2026",
            &["Acme"],
            false,
        );

        assert_eq!(plan, QueryRoutingPlan::legacy_default());
    }
}
