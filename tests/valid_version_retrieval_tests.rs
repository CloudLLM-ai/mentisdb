//! Regression tests for the "agent retrieved the deprecated policy" bug.
//!
//! An agent saved a policy change meant to invalidate the previous policy and
//! linked it to the old memory. Later it pulled the OLD policy. These tests
//! replay that scenario with every way an agent links a replacement (typed
//! `Supersedes`, `Corrects`, kind-less `refs`, `Contradicts`, nothing at all)
//! and assert, through the library, REST and MCP, that the agent gets the new
//! policy first and sees that the old one is superseded or contested.

use mentisdb::{
    AdviceCode, MentisDb, RankedSearchQuery, StorageAdapterKind, Thought, ThoughtInput,
    ThoughtRelation, ThoughtRelationKind, ThoughtType, VersionStatus,
};
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::tempdir;

const OLD_POLICY: &str = "Release policy: we deploy to production on Fridays after lunch.";
/// Shares no words with [`OLD_POLICY`]: a pure paraphrase of the new rule.
const NEW_POLICY: &str = "Ship changes only early in the week and never ahead of a weekend.";
const QUERY_OLD_WORDING: &str = "release policy deploy to production on Fridays";
const QUERY_PARAPHRASE: &str = "can we deploy on Friday afternoon";

#[derive(Clone, Copy, Debug)]
enum Link {
    Supersedes,
    Corrects,
    Invalidates,
    RefsOnlyCorrection,
    RefsOnlyDecision,
    ReferencesRelationAssumption,
    Contradicts,
}

fn rel(kind: ThoughtRelationKind, target: &Thought) -> ThoughtRelation {
    ThoughtRelation {
        kind,
        target_id: target.id,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }
}

fn link_input(link: Link, old: &Thought) -> ThoughtInput {
    let typed = |kind, tt| ThoughtInput::new(tt, NEW_POLICY).with_relations(vec![rel(kind, old)]);
    match link {
        Link::Supersedes => typed(ThoughtRelationKind::Supersedes, ThoughtType::Decision),
        Link::Corrects => typed(ThoughtRelationKind::Corrects, ThoughtType::Correction),
        Link::Invalidates => typed(
            ThoughtRelationKind::Invalidates,
            ThoughtType::AssumptionInvalidated,
        ),
        Link::RefsOnlyCorrection => {
            ThoughtInput::new(ThoughtType::Correction, NEW_POLICY).with_refs(vec![old.index])
        }
        Link::RefsOnlyDecision => {
            ThoughtInput::new(ThoughtType::Decision, NEW_POLICY).with_refs(vec![old.index])
        }
        Link::ReferencesRelationAssumption => {
            ThoughtInput::new(ThoughtType::AssumptionInvalidated, NEW_POLICY)
                .with_relations(vec![rel(ThoughtRelationKind::References, old)])
        }
        Link::Contradicts => typed(ThoughtRelationKind::Contradicts, ThoughtType::Finding),
    }
}

fn is_strong(link: Link) -> bool {
    matches!(link, Link::Supersedes | Link::Corrects | Link::Invalidates)
}

const ALL_LINKS: [Link; 7] = [
    Link::Supersedes,
    Link::Corrects,
    Link::Invalidates,
    Link::RefsOnlyCorrection,
    Link::RefsOnlyDecision,
    Link::ReferencesRelationAssumption,
    Link::Contradicts,
];

fn scenario(link: Link) -> (tempfile::TempDir, MentisDb, Thought, Thought) {
    let dir = tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "policy", StorageAdapterKind::Binary)
            .unwrap();
    let old = chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::Decision, OLD_POLICY).with_importance(0.9),
        )
        .unwrap()
        .clone();
    let new = chain
        .append_thought("agent", link_input(link, &old).with_importance(0.6))
        .unwrap()
        .clone();
    (dir, chain, old, new)
}

/// The new policy must outrank the old one for the old wording and for the
/// paraphrase, whichever way it was linked.
#[test]
fn new_policy_comes_first_for_every_link_style() {
    for link in ALL_LINKS {
        for query in [QUERY_OLD_WORDING, QUERY_PARAPHRASE] {
            let (_dir, chain, old, new) = scenario(link);
            let result = chain.query_ranked(&RankedSearchQuery::new().with_text(query));
            let ids: Vec<_> = result.hits.iter().map(|h| h.thought.id).collect();
            assert_eq!(
                ids.first(),
                Some(&new.id),
                "{link:?} / {query}: new policy must lead"
            );
            let new_hit = &result.hits[0];
            assert_eq!(
                new_hit.version.successor_of,
                vec![old.id],
                "{link:?}: stands in for old"
            );
            if is_strong(link) {
                assert!(
                    !ids.contains(&old.id),
                    "{link:?}: superseded old policy must be hidden"
                );
                assert_eq!(chain.version_status(old.id), VersionStatus::Superseded);
            } else {
                let old_hit = result.hits.iter().find(|h| h.thought.id == old.id).unwrap();
                assert_eq!(old_hit.version.status, VersionStatus::Contested, "{link:?}");
                assert_eq!(old_hit.version.successors[0].thought_id, new.id);
            }
        }
    }
}

/// The `include_invalidated=true` retry the skill used to recommend must still
/// tell the agent the old policy is superseded, and by what.
#[test]
fn include_invalidated_retry_marks_the_old_policy_and_names_the_successor() {
    for link in [Link::Supersedes, Link::Corrects, Link::Invalidates] {
        let (_dir, chain, old, new) = scenario(link);
        let result = chain.query_ranked(
            &RankedSearchQuery::new()
                .with_text(QUERY_OLD_WORDING)
                .with_include_invalidated(true),
        );
        let old_hit = result
            .hits
            .iter()
            .find(|h| h.thought.id == old.id)
            .expect("old kept for audit");
        assert_eq!(old_hit.version.status, VersionStatus::Superseded);
        assert_eq!(
            old_hit.version.current_version.as_ref().unwrap().thought_id,
            new.id
        );
        let json = chain.hit_json(old_hit);
        assert_eq!(json["version_status"], "superseded");
        assert_eq!(json["current_version"]["thought_id"], new.id.to_string());
    }
}

/// A chain of supersessions resolves to the newest live version, and a cycle
/// cannot hang the walk.
#[test]
fn supersession_chain_resolves_to_the_newest_live_version() {
    let dir = tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "chain3", StorageAdapterKind::Binary)
            .unwrap();
    let v1 = chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Decision, OLD_POLICY))
        .unwrap()
        .clone();
    let v2 = chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Decision, "Deploy on Thursdays only.")
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &v1)]),
        )
        .unwrap()
        .clone();
    let v3 = chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Decision, NEW_POLICY)
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &v2)]),
        )
        .unwrap()
        .clone();
    assert_eq!(chain.current_version(v1.id).unwrap().id, v3.id);
    let result = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY_OLD_WORDING));
    assert_eq!(result.hits.len(), 1, "v1 and v2 collapse into v3");
    assert_eq!(result.hits[0].thought.id, v3.id);
    assert_eq!(result.hits[0].version.successor_of.len(), 2);
}

/// A successor that fails the caller's filter is not smuggled into results.
#[test]
fn redirect_respects_the_callers_filter() {
    let (_dir, chain, _old, _new) = scenario(Link::Corrects);
    let filter = mentisdb::ThoughtQuery::new().with_types(vec![ThoughtType::Decision]);
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(filter)
            .with_text(QUERY_OLD_WORDING),
    );
    assert!(
        result.hits.is_empty(),
        "the Correction successor is filtered out and the old Decision is hidden"
    );
}

/// Resolution can be switched off to get the plain "hide" behaviour.
#[test]
fn resolve_successors_off_only_hides() {
    let (_dir, chain, old, new) = scenario(Link::Supersedes);
    let q = RankedSearchQuery::new()
        .with_text(QUERY_OLD_WORDING)
        .with_resolve_successors(false)
        .with_thesaurus(false);
    let ids: Vec<_> = chain
        .query_ranked(&q)
        .hits
        .iter()
        .map(|h| h.thought.id)
        .collect();
    assert!(
        !ids.contains(&old.id) && !ids.contains(&new.id),
        "paraphrased successor has no lexical match"
    );
}

/// Point-in-time reads before the replacement still return the old policy.
#[test]
fn as_of_before_the_replacement_returns_the_old_policy() {
    let (_dir, chain, old, new) = scenario(Link::Supersedes);
    let as_of = old.timestamp;
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text(QUERY_OLD_WORDING)
            .with_as_of(as_of),
    );
    assert_eq!(result.hits[0].thought.id, old.id);
    assert_eq!(result.hits[0].version.status, VersionStatus::Current);
    assert!(result.hits.iter().all(|h| h.thought.id != new.id));
}

/// Append advice: a Correction with no link names likely targets; a
/// well-linked one is silent.
#[test]
fn append_advice_flags_untyped_corrections_and_stays_silent_when_linked() {
    for link in [
        Link::RefsOnlyCorrection,
        Link::RefsOnlyDecision,
        Link::ReferencesRelationAssumption,
    ] {
        let (_dir, chain, old, new) = scenario(link);
        let advice = chain.append_advice(&new);
        assert_eq!(advice.len(), 1, "{link:?}");
        assert_eq!(advice[0].suggested_targets[0].thought_id, old.id);
        assert!(advice[0].message.contains(&old.id.to_string()));
    }
    for link in [Link::Supersedes, Link::Corrects, Link::Invalidates] {
        let (_dir, chain, _old, new) = scenario(link);
        assert!(chain.append_advice(&new).is_empty(), "{link:?}");
    }
    // No link at all: candidates come from lexical similarity.
    let dir = tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "bare", StorageAdapterKind::Binary)
            .unwrap();
    let old = chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Decision, OLD_POLICY))
        .unwrap()
        .clone();
    let fix = chain
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Correction,
                "The Fridays production deploy policy was wrong.",
            ),
        )
        .unwrap()
        .clone();
    let advice = chain.append_advice(&fix);
    assert_eq!(
        advice[0].code,
        AdviceCode::CorrectionWithoutInvalidatingRelation
    );
    assert_eq!(advice[0].suggested_targets[0].thought_id, old.id);
}

/// Lineage is derived at open time, so chains written before this feature
/// benefit with no migration.
#[test]
fn lineage_is_rebuilt_when_the_chain_is_reopened() {
    let (dir, chain, old, _new) = scenario(Link::RefsOnlyCorrection);
    drop(chain);
    let reopened =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "policy", StorageAdapterKind::Binary)
            .unwrap();
    assert_eq!(reopened.version_status(old.id), VersionStatus::Contested);
}

/// Standing context must not lose a constraint when its replacement is not a
/// standing type, and must flag a merely contested one.
#[test]
fn standing_context_shows_the_successor_not_nothing() {
    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key_and_storage_kind(
        dir.path(),
        "standing",
        StorageAdapterKind::Binary,
    )
    .unwrap();
    let old = chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Constraint, OLD_POLICY))
        .unwrap()
        .clone();
    let fix = chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Correction, NEW_POLICY)
                .with_relations(vec![rel(ThoughtRelationKind::Corrects, &old)]),
        )
        .unwrap()
        .clone();
    let standing = chain.standing_context(&mentisdb::StandingContextQuery::new());
    assert_eq!(standing.entries.len(), 1);
    assert_eq!(standing.entries[0].id, fix.id);
    assert!(standing.prompt.contains(NEW_POLICY) && !standing.prompt.contains(OLD_POLICY));

    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key_and_storage_kind(
        dir.path(),
        "standing2",
        StorageAdapterKind::Binary,
    )
    .unwrap();
    let old = chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Constraint, OLD_POLICY))
        .unwrap()
        .clone();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Correction, NEW_POLICY).with_refs(vec![old.index]),
        )
        .unwrap();
    let standing = chain.standing_context(&mentisdb::StandingContextQuery::new());
    assert_eq!(standing.entries.len(), 2, "both versions are shown");
    assert!(standing.prompt.contains("[CONTESTED"));
}

/// The MEMORY.md export is reloaded as memory: superseded policies are
/// dropped from it and contested ones are marked.
#[test]
fn memory_markdown_export_drops_superseded_and_marks_contested() {
    let (_dir, chain, _old, _new) = scenario(Link::Supersedes);
    let md = chain.to_memory_markdown(None);
    assert!(md.contains(NEW_POLICY) && !md.contains(OLD_POLICY));
    let (_dir, chain, _old, _new) = scenario(Link::RefsOnlyCorrection);
    let md = chain.to_memory_markdown(None);
    assert!(md.contains(OLD_POLICY) && md.contains("contested by #1"));
}

/// Plain `Finding`s that merely reference a decision are not contests.
#[test]
fn ordinary_references_do_not_contest() {
    let dir = tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "plain", StorageAdapterKind::Binary)
            .unwrap();
    let old = chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Decision, OLD_POLICY))
        .unwrap()
        .clone();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Finding, "Friday deploys broke twice.")
                .with_refs(vec![old.index]),
        )
        .unwrap();
    assert_eq!(chain.version_status(old.id), VersionStatus::Current);
}

#[cfg(feature = "server")]
mod transport {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    static N: AtomicU64 = AtomicU64::new(0);

    fn cfg(dir: &std::path::Path) -> MentisDbServiceConfig {
        MentisDbServiceConfig::new(dir.to_path_buf(), "policy-http", StorageAdapterKind::Binary)
    }

    async fn post(router: axum::Router, uri: &str, body: Value) -> Value {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    async fn append(router: &axum::Router, body: Value) -> Value {
        let mut body = body;
        body["chain_key"] = json!("policy-http");
        body["agent_id"] = json!("agent");
        post(router.clone(), "/v1/thoughts", body).await
    }

    /// The exact user scenario over REST and MCP: a Correction linked only by
    /// `refs`, then a typed Supersedes.
    #[tokio::test]
    async fn rest_and_mcp_return_the_new_policy_first_with_annotations() {
        for typed in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "mentisdb_valid_version_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let rest = rest_router(cfg(&dir));
            let mcp = mcp_router(cfg(&dir));
            let old = append(
                &rest,
                json!({"thought_type": "Decision", "content": OLD_POLICY}),
            )
            .await;
            let old_id = old["thought"]["id"].as_str().unwrap().to_string();
            let new_body = if typed {
                json!({"thought_type": "Decision", "content": NEW_POLICY,
                       "relations": [{"kind": "supersedes", "target_id": old_id}]})
            } else {
                json!({"thought_type": "Correction", "content": NEW_POLICY, "refs": [0]})
            };
            let appended = append(&rest, new_body).await;
            // Append-time guidance appears only for the untyped link.
            assert_eq!(
                appended["warnings"].is_array(),
                !typed,
                "typed={typed}: {appended}"
            );
            if !typed {
                assert!(appended["warnings"][0]["message"]
                    .as_str()
                    .unwrap()
                    .contains(&old_id));
            }

            for query in [QUERY_OLD_WORDING, QUERY_PARAPHRASE] {
                let payload = json!({"chain_key": "policy-http", "text": query, "limit": 5});
                let via_rest = post(rest.clone(), "/v1/ranked-search", payload.clone()).await;
                let via_mcp = post(
                    mcp.clone(),
                    "/tools/execute",
                    json!({"tool": "mentisdb_ranked_search", "parameters": payload}),
                )
                .await["result"]["output"]
                    .clone();
                for response in [via_rest, via_mcp] {
                    let first = &response["results"][0]["thought"];
                    assert_eq!(first["content"], NEW_POLICY, "typed={typed} query={query}");
                    assert_eq!(first["successor_of"][0], old_id, "{response}");
                    let old_hit = response["results"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|r| r["thought"]["id"] == old_id);
                    match (typed, old_hit) {
                        (true, hit) => assert!(hit.is_none(), "superseded policy must be hidden"),
                        (false, hit) => {
                            let t = &hit.expect("contested old policy stays visible")["thought"];
                            assert_eq!(t["version_status"], "contested");
                            assert_eq!(t["successors"][0]["kind"], "contests");
                        }
                    }
                }
            }

            if typed {
                let audit = post(
                    rest.clone(),
                    "/v1/ranked-search",
                    json!({"chain_key": "policy-http", "text": QUERY_OLD_WORDING,
                           "limit": 5, "include_invalidated": true}),
                )
                .await;
                let t = audit["results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["thought"]["id"] == old_id)
                    .unwrap();
                assert_eq!(t["thought"]["version_status"], "superseded");
                assert_eq!(t["thought"]["current_version"]["kind"], "supersedes");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Regression: the live invalidation check ignored a relation's `invalid_at`,
/// so an expired supersession still hid its target while the `as_of` check
/// disagreed. Both must say the old policy is current again.
#[test]
fn expired_supersession_no_longer_hides_the_target() {
    let dir = tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "expired", StorageAdapterKind::Binary)
            .unwrap();
    let old = chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Decision, OLD_POLICY))
        .unwrap()
        .clone();
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Decision, NEW_POLICY).with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::Supersedes,
                    target_id: old.id,
                    chain_key: None,
                    valid_at: Some(past - chrono::Duration::hours(1)),
                    invalid_at: Some(past),
                },
            ]),
        )
        .unwrap();
    assert!(!chain.is_invalidated(old.id));
    assert_eq!(
        chain.is_invalidated(old.id),
        chain.is_invalidated_as_of(old.id, chrono::Utc::now())
    );
    assert_eq!(chain.version_status(old.id), VersionStatus::Current);
}

/// Lineage across chains and in context bundles. A policy on one chain and its
/// replacement on another (a relation carrying the target chain's key) must be
/// resolved wherever one read path sees both chains.
mod cross_chain {
    use super::*;
    use mentisdb::audience::Audience;

    fn two_chains() -> (tempfile::TempDir, MentisDb, MentisDb, Thought, Thought) {
        let dir = tempdir().unwrap();
        let mut parent = MentisDb::open_with_key_and_storage_kind(
            dir.path(),
            "parent",
            StorageAdapterKind::Binary,
        )
        .unwrap();
        let old = parent
            .append_thought("a", ThoughtInput::new(ThoughtType::Decision, OLD_POLICY))
            .unwrap()
            .clone();
        let mut sibling = MentisDb::open_with_key_and_storage_kind(
            dir.path(),
            "sibling",
            StorageAdapterKind::Binary,
        )
        .unwrap();
        let new = sibling
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::Decision, NEW_POLICY).with_relations(vec![
                    ThoughtRelation {
                        kind: ThoughtRelationKind::Supersedes,
                        target_id: old.id,
                        chain_key: Some("parent".into()),
                        valid_at: None,
                        invalid_at: None,
                    },
                ]),
            )
            .unwrap()
            .clone();
        (dir, parent, sibling, old, new)
    }

    /// The library federated search redirects the superseded hit to its
    /// successor on the other chain; include_invalidated keeps the old hit.
    #[test]
    fn library_federated_search_redirects_across_chains() {
        let (_d, parent, sibling, old, new) = two_chains();
        let q = RankedSearchQuery::new().with_text(QUERY_OLD_WORDING);
        let r = parent.query_federated(
            "parent",
            &[("sibling", &sibling)],
            &q,
            &Default::default(),
            10,
            0,
        );
        assert_eq!(r.hits.len(), 1);
        assert_eq!(
            (r.hits[0].chain_key, r.hits[0].thought.id),
            ("sibling", new.id)
        );
        // Searching the parent alone cannot see the replacement: unchanged.
        let alone = parent.query_ranked(&q);
        assert_eq!(alone.hits[0].thought.id, old.id);
        let audit = q.clone().with_include_invalidated(true);
        let r = parent.query_federated(
            "parent",
            &[("sibling", &sibling)],
            &audit,
            &Default::default(),
            10,
            0,
        );
        assert!(r.hits.iter().any(|h| h.thought.id == old.id));
    }

    /// An edge whose source the caller may not see is ignored.
    #[test]
    fn cross_chain_successor_requires_a_visible_source() {
        let (_d, parent, sibling, old, new) = two_chains();
        let both = [
            ("parent", &parent, Audience::Authorized),
            ("sibling", &sibling, Audience::Authorized),
        ];
        assert_eq!(
            mentisdb::lineage::cross_chain_successor(&both, "parent", old.id)
                .unwrap()
                .thought
                .id,
            new.id
        );
        // The replacement chain is not part of this read path.
        let only = [("parent", &parent, Audience::Authorized)];
        assert!(mentisdb::lineage::cross_chain_successor(&only, "parent", old.id).is_none());
    }

    /// Context bundles redirect a superseded seed to its live successor and put
    /// the contesting thought first for a contested one.
    #[test]
    fn context_bundles_redirect_seeds() {
        for link in [Link::Corrects, Link::RefsOnlyCorrection] {
            let (_d, chain, old, new) = scenario(link);
            let q = RankedSearchQuery::new()
                .with_text(QUERY_OLD_WORDING)
                .with_limit(5);
            let bundles = chain.query_context_bundles(&q).bundles;
            let first = &bundles[0].seed.locator;
            assert_eq!(first.thought_index, Some(new.index), "{link:?}");
            let seeds: Vec<_> = bundles
                .iter()
                .map(|b| b.seed.locator.thought_index)
                .collect();
            assert_eq!(
                seeds.contains(&Some(old.index)),
                !is_strong(link),
                "{link:?}"
            );
        }
    }
}

#[cfg(feature = "server")]
mod cross_chain_transport {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use mentisdb::server::{rest_router, MentisDbServiceConfig};
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    async fn post(router: &axum::Router, uri: &str, body: Value) -> Value {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// Policy on the parent chain, replacement written on a branch and on a
    /// sibling chain: branch search (which includes ancestors) and federated
    /// search both return the replacement, and parent-only search is unchanged.
    #[tokio::test]
    async fn branch_and_federated_search_apply_cross_chain_lineage() {
        let dir = std::env::temp_dir().join(format!("mentisdb_cross_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rest = rest_router(MentisDbServiceConfig::new(
            dir.clone(),
            "parent",
            StorageAdapterKind::Binary,
        ));
        let old = post(&rest, "/v1/thoughts", json!({"chain_key":"parent","agent_id":"a","thought_type":"Decision","content":OLD_POLICY})).await;
        let old_id = old["thought"]["id"].as_str().unwrap().to_string();
        let branch = post(
            &rest,
            "/v1/chains/branch",
            json!({"source_chain_key":"parent","branch_thought_id":old_id,"branch_chain_key":"branch"}),
        )
        .await;
        let _ = branch;
        let supersede = json!([{"kind":"supersedes","target_id":old_id,"chain_key":"parent"}]);
        for chain in ["branch", "sibling"] {
            post(&rest, "/v1/thoughts", json!({"chain_key":chain,"agent_id":"a","thought_type":"Decision","content":NEW_POLICY,"relations":supersede})).await;
        }
        let first = |v: &Value| v["results"][0]["thought"]["content"].clone();
        // Branch search includes the parent as an ancestor.
        let r = post(
            &rest,
            "/v1/ranked-search",
            json!({"chain_key":"branch","text":QUERY_OLD_WORDING,"limit":5}),
        )
        .await;
        assert_eq!(first(&r), NEW_POLICY, "{r}");
        assert!(
            r["results"]
                .as_array()
                .unwrap()
                .iter()
                .all(|h| h["thought"]["id"] != old_id.as_str()),
            "old hidden: {r}"
        );
        // Federated across parent and sibling.
        let r = post(
            &rest,
            "/v1/federated-search",
            json!({"chain_keys":["parent","sibling"],"text":QUERY_OLD_WORDING,"limit":5}),
        )
        .await;
        assert_eq!(first(&r), NEW_POLICY, "{r}");
        assert_eq!(r["results"][0]["chain_key"], "sibling");
        assert!(r["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["thought"]["id"] != old_id.as_str()));
        // Audit view keeps the old hit and says what replaced it.
        let r = post(&rest, "/v1/federated-search", json!({"chain_keys":["parent","sibling"],"text":QUERY_OLD_WORDING,"limit":5,"include_invalidated":true})).await;
        let hit = r["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["thought"]["id"] == old_id.as_str())
            .expect("old kept for audit");
        assert_eq!(hit["thought"]["version_status"], "superseded");
        assert_eq!(hit["thought"]["current_version"]["chain_key"], "sibling");
        // The parent alone cannot see either replacement.
        let r = post(
            &rest,
            "/v1/ranked-search",
            json!({"chain_key":"parent","text":QUERY_OLD_WORDING,"limit":5}),
        )
        .await;
        assert_eq!(first(&r), OLD_POLICY);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
