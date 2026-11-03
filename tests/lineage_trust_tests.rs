//! Who may replace whose memory.
//!
//! The valid-version fix redirects a superseded thought to its live successor.
//! These tests pin the three rules that keep that from becoming a way to
//! poison someone else's memory: on an open chain any writer may replace any
//! thought but the replacement says who wrote it and whether that is
//! established; on a governed chain only the same agent or an authorized key
//! replaces, anyone else only contests; and a version chain has a length limit.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::provenance::ProvenanceTrust;
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{
    MentisDb, PublicKeyAlgorithm, RankedSearchQuery, StandingContextQuery, StorageAdapterKind,
    Thought, ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType, VersionStatus,
    MAX_LINEAGE_DEPTH,
};
use std::io::ErrorKind;
use tempfile::{tempdir, TempDir};

const OLD: &str = "Release policy: we deploy to production on Fridays after lunch.";
/// Shares no words with [`OLD`].
const NEW: &str = "Ship changes only early in the week and never ahead of a weekend.";
const QUERY: &str = "release policy deploy to production on Fridays";

fn rel(kind: ThoughtRelationKind, target: &Thought) -> ThoughtRelation {
    ThoughtRelation {
        kind,
        target_id: target.id,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }
}

fn open() -> (MentisDb, TempDir) {
    let dir = tempdir().unwrap();
    let chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "lin", StorageAdapterKind::Binary)
            .unwrap();
    (chain, dir)
}

fn top_hit(chain: &MentisDb, query: &str) -> (String, mentisdb::VersionInfo) {
    let result = chain.query_ranked(&RankedSearchQuery::new().with_text(query).with_limit(5));
    let hit = result.hits.first().expect("a hit");
    (hit.thought.content.clone(), hit.version.clone())
}

// ---------------------------------------------------------------- open chain

/// Product constraint of the release: on an open chain a correction from a
/// *different* agent still redirects (several sessions share one chain).
#[test]
fn open_chain_cross_agent_correction_still_redirects_and_says_who() {
    let (mut chain, _dir) = open();
    let old = chain
        .append("alice", ThoughtType::Constraint, OLD)
        .unwrap()
        .clone();
    let new = chain
        .append_thought(
            "bob",
            ThoughtInput::new(ThoughtType::Constraint, NEW)
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &old)]),
        )
        .unwrap()
        .clone();
    assert_eq!(chain.current_version(old.id).unwrap().id, new.id);
    assert_eq!(chain.version_status(old.id), VersionStatus::Superseded);

    let (content, version) = top_hit(&chain, QUERY);
    assert_eq!(content, NEW, "the replacement takes the slot");
    assert_eq!(version.successor_of, vec![old.id]);
    assert!(
        version.cross_agent,
        "the hit says it replaced another agent"
    );

    // The superseded thought, read directly, names the replacement's writer.
    let info = chain.version_info(&old, None);
    let current = info.current_version.expect("current version");
    assert_eq!(current.agent_id, "bob");
    assert_eq!(current.trust, ProvenanceTrust::Unsigned);
    assert!(current.cross_agent && current.authorized);
    assert!(info.cross_agent);
    assert!(info.successors.iter().all(|n| n.agent_id == "bob"));

    // And so does the JSON an agent reads.
    let result = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY));
    let json = chain.hit_json(&result.hits[0]);
    assert_eq!(json["cross_agent"], true);
    assert_eq!(json["agent_id"], "bob");
    assert_eq!(json["provenance"]["signed"], false);
    let superseded = chain.thought_json(&old);
    assert_eq!(superseded["current_version"]["agent_id"], "bob");
    assert_eq!(superseded["cross_agent"], true);
}

#[test]
fn open_chain_same_agent_correction_is_not_flagged_cross_agent() {
    let (mut chain, _dir) = open();
    let old = chain
        .append("alice", ThoughtType::Constraint, OLD)
        .unwrap()
        .clone();
    chain
        .append_thought(
            "alice",
            ThoughtInput::new(ThoughtType::Constraint, NEW)
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &old)]),
        )
        .unwrap();
    let (content, version) = top_hit(&chain, QUERY);
    assert_eq!(content, NEW);
    assert!(!version.cross_agent);
    assert!(!chain
        .thought_json(&old)
        .as_object()
        .unwrap()
        .contains_key("cross_agent"));
}

#[test]
fn standing_context_calls_out_a_cross_agent_replacement() {
    let (mut chain, _dir) = open();
    let old = chain
        .append("alice", ThoughtType::Constraint, OLD)
        .unwrap()
        .clone();
    chain
        .append_thought(
            "bob",
            ThoughtInput::new(ThoughtType::Constraint, NEW)
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &old)]),
        )
        .unwrap();
    let standing = chain.standing_context(&StandingContextQuery::new());
    assert_eq!(standing.entries.len(), 1);
    let entry = &standing.entries[0];
    assert_eq!(entry.content, NEW);
    assert_eq!(entry.agent_id, "bob");
    assert_eq!(entry.replaces, Some(old.index));
    assert!(entry.cross_agent);
    assert!(
        standing.prompt.contains("replaced by another agent"),
        "{}",
        standing.prompt
    );
    assert!(standing.prompt.contains("trust=unsigned"));
}

// ------------------------------------------------------------- governed chain

const GOV_SEED: [u8; 32] = [7; 32];
const B_SEED: [u8; 32] = [9; 32];

fn key(seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(seed)
}

fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

fn register(chain: &mut MentisDb, agent: &str, key_id: &str, seed: &[u8; 32]) {
    chain
        .upsert_agent(agent, Some(agent), None, None, None)
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    let public = key(seed).verifying_key().to_bytes().to_vec();
    let payload = key_registration_payload(chain.chain_key(), agent, key_id, &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key(seed).sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(agent, key_id, PublicKeyAlgorithm::Ed25519, public, &proof)
        .unwrap();
}

/// Append a policy record (signed by the governor) allowing `allowed`.
fn set_policy(chain: &mut MentisDb, mode: AccessMode, allowed: &[(&str, &str, &[u8; 32])]) {
    let prev = chain
        .access_policy()
        .head_record_hash
        .clone()
        .unwrap_or_default();
    let seq = chain.access_policy().seq.map_or(0, |s| s + 1);
    let keys = allowed
        .iter()
        .map(|(agent, key_id, seed)| {
            AllowedKey::new(agent, key_id, &key(seed).verifying_key().to_bytes())
        })
        .collect();
    let record = PolicyRecord::new(seq, mode, keys, &prev).unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        "gov",
        &head(chain),
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&key(&GOV_SEED).sign(&payload).to_bytes());
    chain
        .set_access_policy("gov", &record, "gov-1", signature)
        .unwrap();
}

fn signed_as(
    chain: &mut MentisDb,
    agent: &str,
    key_id: &str,
    seed: &[u8; 32],
    mut input: ThoughtInput,
) -> Thought {
    let payload = thought_payload_v2(chain.chain_key(), agent, &head(chain), &input);
    input.signing_key_id = Some(key_id.into());
    input.thought_signature = Some(StoredSignature::encode_v2(
        &key(seed).sign(&payload).to_bytes(),
    ));
    chain.append_thought(agent, input).unwrap().clone()
}

/// An allowlist chain admitting `gov` and `b`.
fn governed() -> (MentisDb, TempDir) {
    let (mut chain, dir) = open();
    register(&mut chain, "gov", "gov-1", &GOV_SEED);
    register(&mut chain, "b", "b-1", &B_SEED);
    set_policy(
        &mut chain,
        AccessMode::Allowlist,
        &[("gov", "gov-1", &GOV_SEED), ("b", "b-1", &B_SEED)],
    );
    (chain, dir)
}

fn supersedes(content: &str, old: &Thought) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Decision, content)
        .with_relations(vec![rel(ThoughtRelationKind::Supersedes, old)])
}

#[test]
fn governed_chain_authorized_key_replaces_across_agents() {
    let (mut chain, _dir) = governed();
    let old = signed_as(
        &mut chain,
        "gov",
        "gov-1",
        &GOV_SEED,
        ThoughtInput::new(ThoughtType::Decision, OLD),
    );
    let new = signed_as(&mut chain, "b", "b-1", &B_SEED, supersedes(NEW, &old));
    assert!(chain.strong_link_is_authorized(new.index, old.id));
    assert_eq!(chain.version_status(old.id), VersionStatus::Superseded);
    assert_eq!(chain.current_version(old.id).unwrap().id, new.id);
    let (content, version) = top_hit(&chain, QUERY);
    assert_eq!(content, NEW);
    assert!(version.cross_agent);
    let current = chain.version_info(&old, None).current_version.unwrap();
    assert_eq!(current.agent_id, "b");
    assert_eq!(current.trust, ProvenanceTrust::Verified);
    assert!(current.authorized);
}

/// A link by a writer who is no longer authorized on the chain only contests;
/// the same writer still replaces their own thoughts.
#[test]
fn governed_chain_unauthorized_writer_only_contests() {
    let (mut chain, _dir) = governed();
    let gov_old = signed_as(
        &mut chain,
        "gov",
        "gov-1",
        &GOV_SEED,
        ThoughtInput::new(ThoughtType::Decision, OLD),
    );
    let b_old = signed_as(
        &mut chain,
        "b",
        "b-1",
        &B_SEED,
        ThoughtInput::new(ThoughtType::Decision, "Backups run nightly at two."),
    );
    let attack = signed_as(&mut chain, "b", "b-1", &B_SEED, supersedes(NEW, &gov_old));
    let own = signed_as(
        &mut chain,
        "b",
        "b-1",
        &B_SEED,
        supersedes("Backups run hourly now.", &b_old),
    );
    // While authorized, both replace.
    assert_eq!(chain.version_status(gov_old.id), VersionStatus::Superseded);

    // The operator removes b from the allowlist.
    set_policy(
        &mut chain,
        AccessMode::Allowlist,
        &[("gov", "gov-1", &GOV_SEED)],
    );

    assert!(!chain.strong_link_is_authorized(attack.index, gov_old.id));
    assert_eq!(chain.version_status(gov_old.id), VersionStatus::Contested);
    assert!(!chain.is_invalidated(gov_old.id));
    assert!(chain.current_version(gov_old.id).is_none());
    let (content, version) = top_hit(&chain, QUERY);
    assert_eq!(content, OLD, "the original stays current");
    assert!(version.successor_of.is_empty());
    let info = chain.version_info(&gov_old, None);
    let note = info
        .successors
        .iter()
        .find(|n| n.thought_id == attack.id)
        .unwrap();
    assert!(
        !note.authorized,
        "the link is shown but marked unauthorized"
    );
    assert_eq!(note.agent_id, "b");
    assert!(note.cross_agent);
    assert_eq!(chain.thought_json(&gov_old)["version_status"], "contested");

    // b's replacement of b's own thought still counts: same agent.
    assert!(chain.strong_link_is_authorized(own.index, b_old.id));
    assert_eq!(chain.version_status(b_old.id), VersionStatus::Superseded);
    assert_eq!(chain.current_version(b_old.id).unwrap().id, own.id);
}

/// The weak link a plain reference makes is not promoted over its target when
/// its writer is not authorized either.
#[test]
fn governed_chain_unauthorized_writer_is_not_promoted_as_a_contester() {
    let (mut chain, _dir) = governed();
    let old = signed_as(
        &mut chain,
        "gov",
        "gov-1",
        &GOV_SEED,
        ThoughtInput::new(ThoughtType::Constraint, OLD),
    );
    let contester = signed_as(
        &mut chain,
        "b",
        "b-1",
        &B_SEED,
        ThoughtInput::new(ThoughtType::Decision, NEW).with_refs(vec![old.index]),
    );
    // Authorized: promoted, and standing context carries it.
    let result = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY));
    assert_eq!(result.hits[0].thought.id, contester.id);
    assert!(chain
        .standing_context(&StandingContextQuery::new())
        .entries
        .iter()
        .any(|e| e.id == contester.id));

    set_policy(
        &mut chain,
        AccessMode::Allowlist,
        &[("gov", "gov-1", &GOV_SEED)],
    );

    let result = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY));
    assert_eq!(result.hits[0].thought.id, old.id, "no promotion");
    assert_eq!(chain.version_status(old.id), VersionStatus::Contested);
    let standing = chain.standing_context(&StandingContextQuery::new());
    let constraint_group: Vec<_> = standing
        .entries
        .iter()
        .filter(|e| e.thought_type == ThoughtType::Constraint)
        .collect();
    assert_eq!(constraint_group.len(), 1);
    assert_eq!(constraint_group[0].id, old.id);
    // Neither is it offered as a replacement of the constraint.
    assert!(
        !standing.prompt.contains("[REPLACES"),
        "{}",
        standing.prompt
    );
    assert!(
        !standing.prompt.contains("[CONTESTED"),
        "{}",
        standing.prompt
    );
}

/// Open eras of a chain that later became governed are not retroactively
/// untrusted: there was no boundary to cross when they were written.
#[test]
fn replacements_written_before_the_chain_was_governed_still_redirect() {
    let (mut chain, _dir) = open();
    let old = chain
        .append("alice", ThoughtType::Constraint, OLD)
        .unwrap()
        .clone();
    let new = chain
        .append_thought("bob", supersedes(NEW, &old))
        .unwrap()
        .clone();
    register(&mut chain, "gov", "gov-1", &GOV_SEED);
    set_policy(
        &mut chain,
        AccessMode::Allowlist,
        &[("gov", "gov-1", &GOV_SEED)],
    );
    assert!(chain.strong_link_is_authorized(new.index, old.id));
    assert_eq!(chain.current_version(old.id).unwrap().id, new.id);
}

// ------------------------------------------------------------- chain length

#[test]
fn a_version_chain_longer_than_the_limit_is_refused_at_write_time() {
    let (mut chain, _dir) = open();
    let mut versions = vec![chain
        .append("a", ThoughtType::Decision, "version 1 of the policy")
        .unwrap()
        .clone()];
    for n in 2..=MAX_LINEAGE_DEPTH {
        let previous = versions.last().unwrap().clone();
        let next = chain
            .append_thought(
                "a",
                supersedes(&format!("version {n} of the policy"), &previous),
            )
            .unwrap_or_else(|e| panic!("version {n} must be accepted: {e}"))
            .clone();
        versions.push(next);
    }
    // The longest allowed chain still resolves end to end.
    assert_eq!(
        chain.current_version(versions[0].id).unwrap().id,
        versions.last().unwrap().id
    );
    let before = chain.thoughts().len();
    let error = chain
        .append_thought(
            "a",
            supersedes("version 65 of the policy", versions.last().unwrap()),
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(error.to_string().contains("limit of 64"), "{error}");
    assert_eq!(chain.thoughts().len(), before, "nothing was written");

    // A plain reference, or a replacement that targets an older, shorter
    // branch, is still fine.
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Decision, "restated policy")
                .with_refs(vec![versions.last().unwrap().index]),
        )
        .unwrap();
    chain
        .append_thought("a", supersedes("branch off version 3", &versions[2]))
        .unwrap();
}

#[test]
fn a_wide_fan_in_does_not_count_as_a_long_chain() {
    let (mut chain, _dir) = open();
    let root = chain
        .append("a", ThoughtType::Decision, "root policy")
        .unwrap()
        .clone();
    for n in 0..(MAX_LINEAGE_DEPTH + 10) {
        chain
            .append_thought("a", supersedes(&format!("sibling {n}"), &root))
            .unwrap();
    }
}
