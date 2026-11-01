//! Public/private segments: a public reader sees only thoughts written while
//! the chain accepted unsigned writes, on every library read surface.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::audience::{Audience, SegmentKind};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{
    MentisDb, PublicKeyAlgorithm, RankedSearchGraph, RankedSearchQuery, StandingContextQuery,
    StorageAdapterKind, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
    ThoughtTraversalAnchor, ThoughtTraversalDirection, ThoughtTraversalRequest, ThoughtType,
};
use uuid::Uuid;

const PUB: Audience = Audience::Public;
const ALL: Audience = Audience::Authorized;

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

fn register_governor(chain: &mut MentisDb) {
    if chain.agent_registry().agents.contains_key("gov") {
        return;
    }
    chain
        .upsert_agent("gov", Some("gov"), None, None, None)
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    let public = key().verifying_key().to_bytes().to_vec();
    let payload = key_registration_payload(chain.chain_key(), "gov", "gov-1", &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key().sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key("gov", "gov-1", PublicKeyAlgorithm::Ed25519, public, &proof)
        .unwrap();
}

fn set_mode(chain: &mut MentisDb, mode: AccessMode) {
    register_governor(chain);
    let prev = chain
        .access_policy()
        .head_record_hash
        .clone()
        .unwrap_or_default();
    let seq = chain.access_policy().seq.map_or(0, |s| s + 1);
    let record = PolicyRecord::new(
        seq,
        mode,
        vec![AllowedKey::new(
            "gov",
            "gov-1",
            &key().verifying_key().to_bytes(),
        )],
        &prev,
    )
    .unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        "gov",
        &head(chain),
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&key().sign(&payload).to_bytes());
    chain
        .set_access_policy("gov", &record, "gov-1", signature)
        .unwrap();
}

fn signed(chain: &mut MentisDb, mut input: ThoughtInput) {
    let payload = thought_payload_v2(chain.chain_key(), "gov", &head(chain), &input);
    input.signing_key_id = Some("gov-1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(&key().sign(&payload).to_bytes()));
    chain.append_thought("gov", input).unwrap();
}

struct Fixture {
    chain: MentisDb,
    dir: std::path::PathBuf,
    /// ids by label.
    ids: std::collections::BTreeMap<&'static str, Uuid>,
}

/// index 0-1 public, 2 policy(private), 3-4 private, 5 policy(open; closes the
/// private segment and is itself private), 6-7 public.
fn fixture(name: &str) -> Fixture {
    fixture_with(
        name,
        "SECRET zebra launch codes for the pelican project",
        "SECRET never reveal the zebra plan",
    )
}

fn fixture_with(name: &str, secret_a: &str, secret_b: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("mentisdb_aud_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    let mut ids = std::collections::BTreeMap::new();
    chain
        .append(
            "pub",
            ThoughtType::Insight,
            "pelican migration notes are public",
        )
        .unwrap();
    ids.insert("p0", chain.thoughts()[0].id);
    chain
        .append(
            "pub",
            ThoughtType::Decision,
            "we chose postgres for the pelican registry",
        )
        .unwrap();
    ids.insert("p1", chain.thoughts()[1].id);
    set_mode(&mut chain, AccessMode::Allowlist);
    ids.insert("rec_private", chain.thoughts()[2].id);
    signed(
        &mut chain,
        ThoughtInput::new(ThoughtType::Insight, secret_a)
            .with_refs(vec![0])
            .with_relations(vec![ThoughtRelation::new(
                ThoughtRelationKind::RelatedTo,
                ids["p1"],
            )]),
    );
    ids.insert("s3", chain.thoughts()[3].id);
    signed(
        &mut chain,
        ThoughtInput::new(ThoughtType::Constraint, secret_b).with_relations(vec![
            ThoughtRelation::new(ThoughtRelationKind::RelatedTo, ids["s3"]),
        ]),
    );
    ids.insert("s4", chain.thoughts()[4].id);
    set_mode(&mut chain, AccessMode::Open);
    ids.insert("rec_open", chain.thoughts()[5].id);
    // A thought in the public segment that links straight into the private
    // one and to a public thought on the far side of it. It is written by an
    // authorized signer (an unsigned writer is refused, see below), which is
    // how older chains came to hold such links.
    signed(
        &mut chain,
        ThoughtInput::new(ThoughtType::Insight, "pelican follow up after the lockdown")
            .with_refs(vec![3, 1])
            .with_relations(vec![
                ThoughtRelation::new(ThoughtRelationKind::RelatedTo, ids["s4"]),
                ThoughtRelation::new(ThoughtRelationKind::RelatedTo, ids["p1"]),
            ]),
    );
    ids.insert("p6", chain.thoughts()[6].id);
    chain
        .append("pub", ThoughtType::Insight, "another public pelican fact")
        .unwrap();
    ids.insert("p7", chain.thoughts()[7].id);
    Fixture { chain, dir, ids }
}

fn indices<'a>(thoughts: impl IntoIterator<Item = &'a mentisdb::Thought>) -> Vec<u64> {
    thoughts.into_iter().map(|t| t.index).collect()
}

#[test]
fn segments_are_derived_from_records() {
    let f = fixture("segments");
    let segments = f.chain.segments().segments();
    assert_eq!(segments.len(), 2);
    assert_eq!(
        (segments[0].start_index, segments[0].kind),
        (2, SegmentKind::Private)
    );
    assert_eq!(
        (segments[1].start_index, segments[1].kind),
        (6, SegmentKind::Public)
    );
    // Governance records are private even when they open a public segment.
    assert_eq!(f.chain.segments().records(), &[2, 5]);
    assert!(f.chain.is_visible_index(1, PUB));
    assert!(!f.chain.is_visible_index(2, PUB));
    assert!(!f.chain.is_visible_index(4, PUB));
    assert!(!f.chain.is_visible_index(5, PUB));
    assert!(f.chain.is_visible_index(6, PUB));
    assert!(f.chain.is_visible_index(4, ALL));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn segments_survive_reopen() {
    let f = fixture("reopen");
    let dir = f.dir.clone();
    drop(f.chain);
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    assert_eq!(chain.segments().segments().len(), 2);
    assert!(!chain.is_visible_index(3, PUB));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_hides_private_thoughts() {
    let f = fixture("query");
    let public = f.chain.query(&ThoughtQuery::new().with_audience(PUB));
    assert_eq!(indices(public), vec![0, 1, 6, 7]);
    let all = f.chain.query(&ThoughtQuery::new());
    assert_eq!(all.len(), 8);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn query_limit_and_text_filter_never_surface_hidden_thoughts() {
    let f = fixture("query_text");
    let q = ThoughtQuery::new().with_text("zebra").with_audience(PUB);
    assert!(f.chain.query(&q).is_empty());
    let q = ThoughtQuery::new()
        .with_types(vec![ThoughtType::Constraint])
        .with_audience(PUB);
    assert!(f.chain.query(&q).is_empty());
    let q = ThoughtQuery::new().with_limit(2).with_audience(PUB);
    assert_eq!(indices(f.chain.query(&q)), vec![6, 7]);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn ranked_search_hides_private_and_counts_only_visible() {
    let f = fixture("ranked");
    let ranked = |audience| {
        f.chain.query_ranked(
            &RankedSearchQuery::new()
                .with_filter(ThoughtQuery::new().with_audience(audience))
                .with_text("zebra pelican")
                .with_limit(50),
        )
    };
    let public = ranked(PUB);
    assert!(public
        .hits
        .iter()
        .all(|h| !h.thought.content.contains("SECRET")));
    assert!(public.hits.iter().all(|h| h.thought.index != 2));
    assert_eq!(public.total_candidates, public.hits.len());
    let all = ranked(ALL);
    assert!(all
        .hits
        .iter()
        .any(|h| h.thought.content.contains("SECRET")));
    // A term that appears only in the private segment matches nothing.
    let only_private = f.chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_audience(PUB))
            .with_text("zebra launch codes"),
    );
    assert!(only_private.hits.is_empty());
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// Scores seen by a public reader must not depend on what is in the private
/// segment: same public thoughts and records, different private text, same scores.
#[test]
fn ranked_scores_do_not_depend_on_private_corpus() {
    let a = fixture("score_a");
    let b = fixture_with(
        "score_b",
        "pelican pelican pelican registry registry zebra totally different private words here",
        "pelican registry pelican registry pelican",
    );
    let query = RankedSearchQuery::new()
        .with_filter(ThoughtQuery::new().with_audience(PUB))
        .with_text("pelican registry")
        .with_limit(50);
    let lexical = |chain: &MentisDb| {
        let r = chain.query_ranked(&query);
        let mut v: Vec<(String, u32)> = r
            .hits
            .iter()
            .map(|h| (h.thought.content.clone(), (h.score.lexical * 1000.0) as u32))
            .collect();
        v.sort();
        v
    };
    assert_eq!(lexical(&a.chain), lexical(&b.chain));
    // Without isolation the same query for an authorized reader does differ.
    let all = |chain: &MentisDb| {
        chain
            .query_ranked(
                &RankedSearchQuery::new()
                    .with_text("pelican registry")
                    .with_limit(50),
            )
            .hits
            .iter()
            .map(|h| (h.score.lexical * 1000.0) as u32)
            .sum::<u32>()
    };
    assert_ne!(all(&a.chain), all(&b.chain));
    let _ = std::fs::remove_dir_all(&a.dir);
    let _ = std::fs::remove_dir_all(&b.dir);
}

/// A graph path may not run through a private thought.
#[test]
fn graph_expansion_never_passes_through_private_thoughts() {
    let f = fixture("graph");
    let query = |audience| {
        RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_audience(audience))
            .with_text("postgres registry")
            .with_graph(
                RankedSearchGraph::new()
                    .with_max_depth(4)
                    .with_max_visited(50),
            )
            .with_limit(50)
    };
    let public = f.chain.query_ranked(&query(PUB));
    let private_ids: Vec<Uuid> = ["rec_private", "s3", "s4"]
        .iter()
        .map(|k| f.ids[k])
        .collect();
    for hit in &public.hits {
        assert!(hit.thought.index != 3 && hit.thought.index != 4);
        // p0 is linked to the seed (p1) only through a private thought.
        if hit.thought.index == 0 {
            assert!(
                hit.graph_distance.is_none(),
                "p0 reached through a private thought"
            );
        }
        if let Some(path) = &hit.graph_path {
            let text = format!("{path:?}");
            for id in &private_ids {
                assert!(
                    !text.contains(&id.to_string()),
                    "private id in graph path: {text}"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn recency_of_public_hits_ignores_private_appends() {
    let mut f = fixture("recency");
    let q = RankedSearchQuery::new()
        .with_filter(ThoughtQuery::new().with_audience(PUB))
        .with_limit(50);
    let before: Vec<f32> = f
        .chain
        .query_ranked(&q)
        .hits
        .iter()
        .map(|h| h.score.recency)
        .collect();
    set_mode(&mut f.chain, AccessMode::Allowlist);
    for i in 0..5 {
        signed(
            &mut f.chain,
            ThoughtInput::new(ThoughtType::Insight, format!("private {i}")),
        );
    }
    let after = f.chain.query_ranked(&q);
    // The private tail does not shift the public hits' recency anchor.
    let newest_public = f.chain.visible_thoughts(PUB).next_back().unwrap().index;
    assert_eq!(newest_public, 7);
    let recency: Vec<f32> = after
        .hits
        .iter()
        .filter(|h| h.thought.index <= 7)
        .map(|h| h.score.recency)
        .collect();
    assert_eq!(before.len(), recency.len());
    for (a, b) in before.iter().zip(&recency) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn get_thought_is_indistinguishable_from_missing() {
    let f = fixture("get");
    let private = &f.chain.thoughts()[3];
    for anchor in [
        ThoughtTraversalAnchor::Id(private.id),
        ThoughtTraversalAnchor::Hash(private.hash.clone()),
        ThoughtTraversalAnchor::Index(3),
    ] {
        assert!(f.chain.get_thought_for(&anchor, PUB).is_none());
        assert!(f.chain.get_thought_for(&anchor, ALL).is_some());
    }
    // Genesis/Head resolve to the first/last public thought.
    let head = f
        .chain
        .get_thought_for(&ThoughtTraversalAnchor::Head, PUB)
        .unwrap();
    assert_eq!(head.index, 7);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn traversal_skips_private_thoughts_and_private_anchors() {
    let f = fixture("traverse");
    let page = f
        .chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 50,
            filter: ThoughtQuery::new().with_audience(PUB),
        })
        .unwrap();
    assert_eq!(indices(page.thoughts.iter().copied()), vec![0, 1, 6, 7]);
    let private = &f.chain.thoughts()[3];
    let page = f
        .chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(private.id),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 50,
            filter: ThoughtQuery::new().with_audience(PUB),
        })
        .unwrap();
    assert!(page.thoughts.is_empty() && page.anchor.is_none());
    // Backward from Head starts at the last public thought.
    let page = f
        .chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Head,
            direction: ThoughtTraversalDirection::Backward,
            include_anchor: true,
            chunk_size: 50,
            filter: ThoughtQuery::new().with_audience(PUB),
        })
        .unwrap();
    assert_eq!(indices(page.thoughts.iter().copied()), vec![7, 6, 1, 0]);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn redaction_drops_pointers_into_private_segments() {
    let f = fixture("redact");
    let p6 = &f.chain.thoughts()[6];
    let redacted = f.chain.redact_for(p6, PUB);
    assert_eq!(redacted.refs, vec![1]);
    assert!(!redacted.relations.is_empty());
    assert!(redacted
        .relations
        .iter()
        .all(|r| r.target_id == f.ids["p1"]));
    let full = f.chain.redact_for(p6, ALL);
    assert_eq!(full.refs, vec![3, 1]);
    assert!(full.relations.iter().any(|r| r.target_id == f.ids["s4"]));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn resolve_context_does_not_walk_into_private_thoughts() {
    let f = fixture("context");
    let public = f.chain.resolve_context_for(6, PUB);
    assert_eq!(indices(public), vec![1, 6]);
    let all = f.chain.resolve_context_for(6, ALL);
    assert!(all.iter().any(|t| t.index == 3));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn standing_and_summary_and_bundles_respect_the_audience() {
    let f = fixture("surfaces");
    let standing = f.chain.standing_context(
        &StandingContextQuery::new()
            .with_types(vec![ThoughtType::Constraint, ThoughtType::Decision])
            .with_audience(PUB),
    );
    assert!(standing.entries.iter().all(|e| e.index != 4));
    assert!(!standing.prompt.contains("SECRET"));
    let all = f.chain.standing_context(
        &StandingContextQuery::new()
            .with_types(vec![ThoughtType::Constraint, ThoughtType::Decision])
            .with_audience(ALL),
    );
    assert!(all.prompt.contains("SECRET"));

    let summaries = f.chain.summary_candidates_for(Default::default(), PUB);
    for candidate in &summaries {
        let text = format!("{candidate:?}");
        assert!(!text.contains(&f.ids["s3"].to_string()));
        assert!(!text.contains(&f.ids["s4"].to_string()));
    }

    let bundles = f.chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_audience(PUB))
            .with_text("zebra pelican")
            .with_graph(RankedSearchGraph::new().with_max_depth(3))
            .with_limit(10),
    );
    let text = format!("{bundles:?}");
    for id in ["s3", "s4", "rec_private"] {
        assert!(!text.contains(&f.ids[id].to_string()));
    }
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn markdown_export_hides_private_thoughts() {
    let f = fixture("markdown");
    let md = f
        .chain
        .to_memory_markdown(Some(&ThoughtQuery::new().with_audience(PUB)));
    assert!(!md.contains("SECRET"));
    assert!(md.contains("pelican"));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn federated_fallback_fails_closed() {
    let f = fixture("federated");
    let mut other_queries = std::collections::BTreeMap::new();
    other_queries.clear();
    // `c2` has no explicit query: the fallback must not inherit Authorized.
    let dir2 = std::env::temp_dir().join(format!("mentisdb_aud_fed2_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir2);
    let c2 =
        MentisDb::open_with_key_and_storage_kind(&dir2, "c2", StorageAdapterKind::Binary).unwrap();
    let query = RankedSearchQuery::new()
        .with_filter(ThoughtQuery::new().with_audience(ALL))
        .with_text("zebra");
    let result = f
        .chain
        .query_federated("c", &[("c2", &c2)], &query, &other_queries, 10, 0);
    assert!(!result.hits.is_empty());
    let _ = std::fs::remove_dir_all(&f.dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// A chain whose policy was already `allowlist` before this model existed is
/// private from the record onwards, with no stored marker.
#[test]
fn existing_policy_chains_migrate_to_private_from_their_record() {
    let dir = std::env::temp_dir().join(format!("mentisdb_aud_migrate_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    chain
        .append("pub", ThoughtType::Insight, "before policy")
        .unwrap();
    set_mode(&mut chain, AccessMode::Signed);
    signed(
        &mut chain,
        ThoughtInput::new(ThoughtType::Insight, "after policy"),
    );
    drop(chain);
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    assert!(chain.is_visible_index(0, PUB));
    assert!(!chain.is_visible_index(1, PUB));
    assert!(!chain.is_visible_index(2, PUB));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── write side: a public writer cannot touch the private segment ────────────

/// An unsigned thought may not point into a private segment (by index or by id).
#[test]
fn public_writer_cannot_link_into_private_segment() {
    let mut f = fixture("private_target");
    let by_ref = f.chain.append_thought(
        "pub",
        ThoughtInput::new(ThoughtType::Insight, "ref probe").with_refs(vec![3]),
    );
    let error = by_ref.expect_err("ref into private segment");
    assert_eq!(
        mentisdb::access::AccessError::from_io(&error),
        Some(&mentisdb::access::AccessError::PrivateTarget)
    );
    let by_relation = f.chain.append_thought(
        "pub",
        ThoughtInput::new(ThoughtType::Insight, "invalidate probe").with_relations(vec![
            ThoughtRelation::new(ThoughtRelationKind::Supersedes, f.ids["s3"]),
        ]),
    );
    assert!(by_relation.is_err());
    // Links to public thoughts keep working.
    f.chain
        .append_thought(
            "pub",
            ThoughtInput::new(ThoughtType::Insight, "fine").with_refs(vec![1]),
        )
        .unwrap();
    // The private thought was not invalidated.
    assert!(!f.chain.is_invalidated(f.ids["s3"]));
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// A signer who may read the private segment can link to it.
#[test]
fn authorized_signer_can_link_into_private_segment() {
    let mut f = fixture("private_target_ok");
    set_mode(&mut f.chain, AccessMode::Allowlist);
    signed(
        &mut f.chain,
        ThoughtInput::new(ThoughtType::Insight, "linked").with_refs(vec![3]),
    );
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// Automatic dedup must not collapse a public write into a private thought.
#[test]
fn dedup_never_supersedes_a_private_thought() {
    let mut f = fixture("dedup");
    f.chain.with_dedup_threshold(Some(0.5));
    let echo = f
        .chain
        .append_thought(
            "pub",
            ThoughtInput::new(
                ThoughtType::Constraint,
                "SECRET never reveal the zebra plan",
            ),
        )
        .unwrap()
        .clone();
    assert!(
        echo.relations
            .iter()
            .all(|r| r.target_id != f.ids["s4"] && r.target_id != f.ids["s3"]),
        "dedup leaked or superseded a private thought: {:?}",
        echo.relations
    );
    assert!(!f.chain.is_invalidated(f.ids["s4"]));
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// Only the key set of the policy, with an active registry key, may read.
#[test]
fn read_authorization_follows_policy_and_registry() {
    let mut f = fixture("readauth");
    set_mode(&mut f.chain, AccessMode::Allowlist);
    let pk = key().verifying_key().to_bytes();
    assert!(f.chain.is_read_authorized("gov", "gov-1", &pk));
    assert!(!f.chain.is_read_authorized("gov", "gov-1", &[1u8; 32]));
    assert!(!f.chain.is_read_authorized("gov", "other", &pk));
    assert!(!f.chain.is_read_authorized("nobody", "gov-1", &pk));
    f.chain.revoke_agent_key("gov", "gov-1").unwrap();
    assert!(!f.chain.is_read_authorized("gov", "gov-1", &pk));
    let _ = std::fs::remove_dir_all(&f.dir);
}

// ── lineage: superseding, contesting and advice never cross the boundary ────

/// public decision D, private decision S that supersedes it, chain reopened.
fn lineage_fixture(name: &str) -> (MentisDb, std::path::PathBuf, Uuid, Uuid) {
    let dir = std::env::temp_dir().join(format!("mentisdb_aud_lin_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    let d = chain
        .append(
            "pub",
            ThoughtType::Decision,
            "deploy the walrus service on fridays",
        )
        .unwrap()
        .id;
    set_mode(&mut chain, AccessMode::Allowlist);
    signed(
        &mut chain,
        ThoughtInput::new(
            ThoughtType::Decision,
            "PRIVATE-REPLACEMENT never deploy the walrus service on fridays",
        )
        .with_relations(vec![ThoughtRelation::new(
            ThoughtRelationKind::Supersedes,
            d,
        )]),
    );
    let s = chain.thoughts().last().unwrap().id;
    set_mode(&mut chain, AccessMode::Open);
    (chain, dir, d, s)
}

#[test]
fn public_reader_still_sees_a_thought_superseded_only_by_a_private_one() {
    let (chain, dir, d, s) = lineage_fixture("super");
    assert!(chain.is_invalidated(d));
    assert!(!chain.is_invalidated_for(d, PUB));
    assert!(chain.is_invalidated_for(d, ALL));
    let ranked = |audience| {
        chain.query_ranked(
            &RankedSearchQuery::new()
                .with_filter(ThoughtQuery::new().with_audience(audience))
                .with_text("walrus fridays")
                .with_limit(10),
        )
    };
    let public = ranked(PUB);
    assert_eq!(public.hits.len(), 1);
    assert_eq!(public.hits[0].thought.id, d);
    let json = chain.hit_json_for(&public.hits[0], PUB).to_string();
    assert!(
        !json.contains(&s.to_string()),
        "private successor leaked: {json}"
    );
    assert!(!json.contains("PRIVATE-REPLACEMENT"));
    assert!(chain
        .version_info_for(&chain.thoughts()[0], None, PUB)
        .is_plain());
    // The authorized reader is redirected to the replacement.
    let all = ranked(ALL);
    assert!(all.hits.iter().any(|h| h.thought.id == s));
    assert!(all.hits.iter().all(|h| h.thought.id != d));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn standing_context_for_public_reader_ignores_private_supersession() {
    let (chain, dir, d, _s) = lineage_fixture("standing");
    let standing =
        |audience| chain.standing_context(&StandingContextQuery::new().with_audience(audience));
    let public = standing(PUB);
    assert!(public.entries.iter().any(|e| e.id == d));
    assert!(!public.prompt.contains("PRIVATE-REPLACEMENT"));
    let all = standing(ALL);
    assert!(all.prompt.contains("PRIVATE-REPLACEMENT"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_advice_does_not_quote_private_thoughts_to_a_public_writer() {
    let (mut chain, dir, _d, _s) = lineage_fixture("advice");
    let fix = chain
        .append_thought(
            "pub",
            ThoughtInput::new(
                ThoughtType::Correction,
                "deploying the walrus service on fridays PRIVATE-REPLACEMENT was wrong",
            ),
        )
        .unwrap()
        .clone();
    let public = serde_json::to_string(&chain.append_advice_for(&fix, PUB)).unwrap();
    assert!(
        !public.contains("PRIVATE-REPLACEMENT"),
        "private excerpt leaked through append advice: {public}"
    );
    let all = serde_json::to_string(&chain.append_advice_for(&fix, ALL)).unwrap();
    assert!(all.contains("PRIVATE-REPLACEMENT"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn provenance_marks_segment_and_signature() {
    let f = fixture("provenance");
    let public = f.chain.thought_json_for(&f.chain.thoughts()[0], PUB);
    assert_eq!(public["provenance"]["segment"], "public");
    assert_eq!(public["provenance"]["signed"], false);
    assert_eq!(public["provenance"]["trust"], "untrusted_data");
    let private = f.chain.thought_json_for(&f.chain.thoughts()[3], ALL);
    assert_eq!(private["provenance"]["segment"], "private");
    assert_eq!(private["provenance"]["signature"], "valid");
    assert_eq!(private["provenance"]["verified_agent"], "gov");
    // A redacted public view of a signed thought still verifies.
    let p6 = f.chain.thought_json_for(&f.chain.thoughts()[6], PUB);
    assert_eq!(p6["provenance"]["signature"], "valid");
    let _ = std::fs::remove_dir_all(&f.dir);
}
