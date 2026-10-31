//! Verify-on-append, signature versions, and chain access policies
//! (`Open`, `Signed`, `Allowlist`) enforced by the library.

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessError, AccessMode, AccessPolicy, AllowedKey, KeyRegistrationProof,
    PolicyRecord,
};
use mentisdb::signing::{
    key_registration_payload, thought_payload_v2, SignatureVersion, StoredSignature,
};
use mentisdb::{
    signable_thought_payload, MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput,
    ThoughtType,
};

fn open(dir_name: &str, chain_key: &str) -> (MentisDb, std::path::PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("mentisdb_access_{dir_name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, chain_key, StorageAdapterKind::Binary)
            .unwrap();
    (chain, dir)
}

fn secret(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn public(key: &SigningKey) -> Vec<u8> {
    key.verifying_key().to_bytes().to_vec()
}

/// Register `key` for `agent` (creating the agent with an unsigned thought if needed).
fn register(chain: &mut MentisDb, agent: &str, key_id: &str, key: &SigningKey) {
    if !chain.agent_registry().agents.contains_key(agent) {
        // Registering an agent needs it to exist; on a restricted chain use upsert.
        chain
            .upsert_agent(agent, Some(agent), None, None, None)
            .unwrap();
    }
    let now = Utc::now().timestamp();
    let payload = key_registration_payload(chain.chain_key(), agent, key_id, &public(key), now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key.sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(
            agent,
            key_id,
            PublicKeyAlgorithm::Ed25519,
            public(key),
            &proof,
        )
        .unwrap();
}

fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

/// Sign `input` with the v2 payload for appending at the chain's current head.
fn sign_v2(
    chain: &MentisDb,
    agent: &str,
    key_id: &str,
    key: &SigningKey,
    mut input: ThoughtInput,
) -> ThoughtInput {
    let payload = thought_payload_v2(chain.chain_key(), agent, &head(chain), &input);
    input.signing_key_id = Some(key_id.to_string());
    input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
    input
}

fn rejection(result: std::io::Result<&mentisdb::Thought>) -> AccessError {
    let error = result.expect_err("append should be refused");
    AccessError::from_io(&error)
        .unwrap_or_else(|| panic!("untyped error: {error}"))
        .clone()
}

fn note(text: &str) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Insight, text)
}

/// Install `record` signed by `signer` through the library.
fn set_policy(
    chain: &mut MentisDb,
    agent: &str,
    key_id: &str,
    key: &SigningKey,
    mode: AccessMode,
    keys: &[(&str, &str, &SigningKey)],
) -> std::io::Result<()> {
    let prev = chain
        .access_policy()
        .head_record_hash
        .clone()
        .unwrap_or_default();
    let seq = chain.access_policy().seq.map_or(0, |s| s + 1);
    let allowed = keys
        .iter()
        .map(|(a, k, sk)| AllowedKey::new(a, k, &public(sk)))
        .collect();
    let record = PolicyRecord::new(seq, mode, allowed, &prev).unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        agent,
        &head(chain),
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&key.sign(&payload).to_bytes());
    chain
        .set_access_policy(agent, &record, key_id, signature)
        .map(|_| ())
}

// ── verify on append ─────────────────────────────────────────────────────────

/// A valid v2 signature is accepted, recorded as v2, and re-verifies later.
#[test]
fn test_valid_v2_signature_is_accepted_and_recorded() {
    let (mut chain, dir) = open("v2ok", "c");
    let key = secret(1);
    register(&mut chain, "alice", "k1", &key);
    let input = sign_v2(&chain, "alice", "k1", &key, note("signed"));
    chain.append_thought("alice", input).unwrap();
    let stored = chain.thoughts().last().unwrap();
    let parsed = StoredSignature::parse(stored.thought_signature.as_deref().unwrap()).unwrap();
    assert_eq!(parsed.version, SignatureVersion::V2);
    let report = chain.verify_signatures();
    assert_eq!((report.signed, report.valid, report.invalid), (1, 1, 0));
    assert_eq!(report.checks.last().unwrap().version, Some(2));
    let _ = std::fs::remove_dir_all(dir);
}

/// Finding 3: signatures were stored unchecked. Now a bad one is refused even on an Open chain.
#[test]
fn test_bad_signatures_are_rejected_on_open_chains() {
    let (mut chain, dir) = open("badsig", "c");
    let key = secret(1);
    register(&mut chain, "alice", "k1", &key);
    // Wrong key.
    let wrong = sign_v2(&chain, "alice", "k1", &secret(2), note("x"));
    assert!(matches!(
        rejection(chain.append_thought("alice", wrong)),
        AccessError::BadSignature(_)
    ));
    // Tampered content after signing.
    let mut tampered = sign_v2(&chain, "alice", "k1", &key, note("original"));
    tampered.content = "changed".into();
    assert!(matches!(
        rejection(chain.append_thought("alice", tampered)),
        AccessError::BadSignature(_)
    ));
    // Claimed agent differs from the signed agent.
    chain
        .upsert_agent("mallory", None, None, None, None)
        .unwrap();
    let theirs = sign_v2(&chain, "alice", "k1", &key, note("x"));
    assert!(chain.append_thought("mallory", theirs).is_err());
    // Missing key id, unknown agent, unknown key, garbage shape.
    let mut no_id = sign_v2(&chain, "alice", "k1", &key, note("x"));
    no_id.signing_key_id = None;
    assert_eq!(
        rejection(chain.append_thought("alice", no_id)),
        AccessError::MissingSigningKeyId
    );
    let unknown_agent = sign_v2(&chain, "ghost", "k1", &key, note("x"));
    assert!(matches!(
        rejection(chain.append_thought("ghost", unknown_agent)),
        AccessError::UnknownAgent(_)
    ));
    let unknown_key = sign_v2(&chain, "alice", "nope", &key, note("x"));
    assert!(matches!(
        rejection(chain.append_thought("alice", unknown_key)),
        AccessError::UnknownKey { .. }
    ));
    let mut garbage = note("x");
    garbage.signing_key_id = Some("k1".into());
    garbage.thought_signature = Some(vec![1, 2, 3]);
    assert!(matches!(
        rejection(chain.append_thought("alice", garbage)),
        AccessError::UnsupportedSignature(_)
    ));
    assert_eq!(
        chain.thoughts().len(),
        0,
        "none of the refused thoughts landed"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Finding 2: a v2 signature is bound to its chain and its position.
#[test]
fn test_v2_signature_cannot_be_replayed_across_chains_or_positions() {
    let (mut one, d1) = open("replay1", "chain-one");
    let (mut two, d2) = open("replay2", "chain-two");
    let key = secret(1);
    for chain in [&mut one, &mut two] {
        register(chain, "alice", "k1", &key);
    }
    let signed = sign_v2(&one, "alice", "k1", &key, note("hello"));
    one.append_thought("alice", signed.clone()).unwrap();
    // Same bytes onto another chain.
    assert!(matches!(
        rejection(two.append_thought("alice", signed.clone())),
        AccessError::BadSignature(_)
    ));
    // Same bytes onto the same chain again (the head moved on): refused, and
    // reported as signed over an earlier head.
    assert_eq!(
        rejection(one.append_thought("alice", signed)),
        AccessError::StaleHead
    );
    let _ = std::fs::remove_dir_all(d1);
    let _ = std::fs::remove_dir_all(d2);
}

/// A revoked key can no longer sign, but what it signed earlier still verifies.
#[test]
fn test_revoked_keys_cannot_sign_but_history_stays_valid() {
    let (mut chain, dir) = open("revoked", "c");
    let key = secret(1);
    register(&mut chain, "alice", "k1", &key);
    let before = sign_v2(&chain, "alice", "k1", &key, note("before"));
    chain.append_thought("alice", before).unwrap();
    chain.revoke_agent_key("alice", "k1").unwrap();
    let after = sign_v2(&chain, "alice", "k1", &key, note("after"));
    assert!(matches!(
        rejection(chain.append_thought("alice", after)),
        AccessError::KeyRevoked { .. }
    ));
    let report = chain.verify_signatures();
    assert_eq!((report.valid, report.invalid), (1, 0));
    let _ = std::fs::remove_dir_all(dir);
}

/// What was signed is exactly what is stored: no automatic relations or valid_at.
#[test]
fn test_signed_thoughts_are_committed_exactly_as_signed() {
    let (mut chain, dir) = open("exact", "c");
    let key = secret(1);
    register(&mut chain, "alice", "k1", &key);
    chain.append_thought("alice", note("target")).unwrap();
    let target = chain.thoughts().last().unwrap().id;
    let mut input = note("refers to target");
    input.refs = vec![0];
    input.relations = vec![mentisdb::ThoughtRelation {
        kind: mentisdb::ThoughtRelationKind::Supports,
        target_id: target,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }];
    chain
        .append_thought("alice", sign_v2(&chain, "alice", "k1", &key, input))
        .unwrap();
    let stored = chain.thoughts().last().unwrap();
    assert_eq!(stored.relations.len(), 1, "no automatic References edge");
    assert!(
        stored.relations[0].valid_at.is_none(),
        "no injected valid_at"
    );
    assert_eq!(chain.verify_signatures().invalid, 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// v1 signatures keep working on Open chains and are reported as v1.
#[test]
fn test_v1_signatures_still_verify_on_open_chains() {
    let (mut chain, dir) = open("v1", "c");
    let key = secret(1);
    register(&mut chain, "alice", "k1", &key);
    let mut input = note("legacy signed");
    let payload = signable_thought_payload("alice", &input);
    input.signing_key_id = Some("k1".into());
    input.thought_signature = Some(key.sign(&payload).to_bytes().to_vec());
    chain.append_thought("alice", input).unwrap();
    let report = chain.verify_signatures();
    assert_eq!(report.checks.last().unwrap().version, Some(1));
    assert_eq!((report.valid, report.invalid), (1, 0));
    let _ = std::fs::remove_dir_all(dir);
}

/// The reserved governance type cannot be appended through the normal path.
#[test]
fn test_access_policy_type_is_reserved() {
    let (mut chain, dir) = open("reserved", "c");
    let error = rejection(
        chain.append_thought("alice", ThoughtInput::new(ThoughtType::AccessPolicy, "{}")),
    );
    assert_eq!(error, AccessError::ReservedThoughtType);
    let _ = std::fs::remove_dir_all(dir);
}

// ── policies ─────────────────────────────────────────────────────────────────

/// Signed chains refuse unsigned thoughts and v1 signatures, and accept v2 from any active key of the agent.
#[test]
fn test_signed_mode_requires_v2_signature_from_an_active_key() {
    let (mut chain, dir) = open("signedmode", "c");
    let (admin, writer) = (secret(1), secret(2));
    register(&mut chain, "admin", "a1", &admin);
    register(&mut chain, "writer", "w1", &writer);
    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Signed,
        &[("admin", "a1", &admin)],
    )
    .unwrap();
    assert_eq!(chain.access_policy().mode, AccessMode::Signed);

    assert_eq!(
        rejection(chain.append_thought("writer", note("unsigned"))),
        AccessError::SignatureRequired
    );
    let mut v1 = note("v1");
    let payload = signable_thought_payload("writer", &v1);
    v1.signing_key_id = Some("w1".into());
    v1.thought_signature = Some(writer.sign(&payload).to_bytes().to_vec());
    assert!(matches!(
        rejection(chain.append_thought("writer", v1)),
        AccessError::SignatureVersionNotAllowed(_)
    ));
    // Any registered key of the agent works in Signed mode (not only governors).
    chain
        .append_thought(
            "writer",
            sign_v2(&chain, "writer", "w1", &writer, note("ok")),
        )
        .unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Allowlist chains admit only listed keys, and revocation takes effect at once.
#[test]
fn test_allowlist_mode_admits_only_listed_active_keys() {
    let (mut chain, dir) = open("allow", "c");
    let (admin, friend, stranger) = (secret(1), secret(2), secret(3));
    register(&mut chain, "admin", "a1", &admin);
    register(&mut chain, "friend", "f1", &friend);
    register(&mut chain, "stranger", "s1", &stranger);
    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Allowlist,
        &[("admin", "a1", &admin), ("friend", "f1", &friend)],
    )
    .unwrap();

    chain
        .append_thought(
            "friend",
            sign_v2(&chain, "friend", "f1", &friend, note("welcome")),
        )
        .unwrap();
    let denied = sign_v2(&chain, "stranger", "s1", &stranger, note("let me in"));
    assert!(matches!(
        rejection(chain.append_thought("stranger", denied)),
        AccessError::KeyNotAllowed { .. }
    ));
    chain.revoke_agent_key("friend", "f1").unwrap();
    let revoked = sign_v2(&chain, "friend", "f1", &friend, note("still here?"));
    assert!(matches!(
        rejection(chain.append_thought("friend", revoked)),
        AccessError::KeyRevoked { .. }
    ));
    let _ = std::fs::remove_dir_all(dir);
}

/// Bootstrap: the genesis record must be signed by a key it lists; later changes need a current governor.
#[test]
fn test_policy_bootstrap_and_governance_rules() {
    let (mut chain, dir) = open("govern", "c");
    let (admin, other, outsider) = (secret(1), secret(2), secret(3));
    // Genesis signed by a key that is not in its own key set.
    let error = set_policy(
        &mut chain,
        "outsider",
        "o1",
        &outsider,
        AccessMode::Allowlist,
        &[("admin", "a1", &admin)],
    )
    .unwrap_err();
    assert!(
        matches!(
            AccessError::from_io(&error),
            Some(AccessError::NotAuthorized(_))
        ),
        "{error}"
    );
    // Signed/allowlist with no keys can never be changed again, so it is refused.
    assert!(PolicyRecord::new(0, AccessMode::Allowlist, vec![], "").is_err());

    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Allowlist,
        &[("admin", "a1", &admin)],
    )
    .unwrap();
    let genesis = chain.access_policy().genesis_hash.clone().unwrap();
    assert_eq!(chain.access_policy().seq, Some(0));

    // A non-governor cannot change it, not even to something stricter.
    let error = set_policy(
        &mut chain,
        "outsider",
        "o1",
        &outsider,
        AccessMode::Open,
        &[("outsider", "o1", &outsider)],
    )
    .unwrap_err();
    assert!(
        matches!(
            AccessError::from_io(&error),
            Some(AccessError::NotAuthorized(_))
        ),
        "{error}"
    );
    // A governor adds another governor; the genesis hash stays.
    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Allowlist,
        &[("admin", "a1", &admin), ("other", "o2", &other)],
    )
    .unwrap();
    assert_eq!(chain.access_policy().seq, Some(1));
    assert_eq!(
        chain.access_policy().genesis_hash.as_deref(),
        Some(genesis.as_str())
    );
    // The new governor can now open the chain again.
    set_policy(
        &mut chain,
        "other",
        "o2",
        &other,
        AccessMode::Open,
        &[("admin", "a1", &admin), ("other", "o2", &other)],
    )
    .unwrap();
    assert_eq!(chain.access_policy().mode, AccessMode::Open);
    chain.append_thought("anyone", note("open again")).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Out-of-sequence, wrongly linked and tampered records are refused.
#[test]
fn test_policy_record_integrity_rules() {
    let (mut chain, dir) = open("integrity", "c");
    let admin = secret(1);
    register(&mut chain, "admin", "a1", &admin);
    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Signed,
        &[("admin", "a1", &admin)],
    )
    .unwrap();
    let sign = |chain: &MentisDb, record: &PolicyRecord| {
        let payload = thought_payload_v2(
            chain.chain_key(),
            "admin",
            &head(chain),
            &policy_thought_input(record),
        );
        StoredSignature::encode_v2(&admin.sign(&payload).to_bytes())
    };
    let keys = vec![AllowedKey::new("admin", "a1", &public(&admin))];
    let prev = chain.access_policy().head_record_hash.clone().unwrap();
    for (name, record) in [
        (
            "seq repeats",
            PolicyRecord::new(0, AccessMode::Open, keys.clone(), &prev).unwrap(),
        ),
        (
            "seq skips",
            PolicyRecord::new(5, AccessMode::Open, keys.clone(), &prev).unwrap(),
        ),
        (
            "prev_policy wrong",
            PolicyRecord::new(1, AccessMode::Open, keys.clone(), "deadbeef").unwrap(),
        ),
    ] {
        let signature = sign(&chain, &record);
        let error = chain
            .set_access_policy("admin", &record, "a1", signature)
            .unwrap_err();
        assert!(
            matches!(
                AccessError::from_io(&error),
                Some(AccessError::PolicyInvalid(_))
            ),
            "{name}: {error}"
        );
    }
    // A wrong keys_root is caught by validation and by canonical parsing.
    let mut forged = PolicyRecord::new(1, AccessMode::Open, keys, &prev).unwrap();
    forged.keys_root = "00".repeat(32);
    assert!(forged.validate().is_err());
    assert!(PolicyRecord::parse(&forged.to_content()).is_err());
    // A signature made for a different head is refused.
    let record = PolicyRecord::new(
        1,
        AccessMode::Open,
        vec![AllowedKey::new("admin", "a1", &public(&admin))],
        &prev,
    )
    .unwrap();
    let stale = sign(&chain, &record);
    chain
        .append_thought(
            "admin",
            sign_v2(&chain, "admin", "a1", &admin, note("moves the head")),
        )
        .unwrap();
    let error = chain
        .set_access_policy("admin", &record, "a1", stale)
        .unwrap_err();
    assert!(
        matches!(
            AccessError::from_io(&error),
            Some(AccessError::BadSignature(_))
        ),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The policy survives closing and reopening the chain because it is replayed from the records.
#[test]
fn test_policy_is_replayed_on_open() {
    let (mut chain, dir) = open("replayopen", "c");
    let admin = secret(1);
    register(&mut chain, "admin", "a1", &admin);
    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Allowlist,
        &[("admin", "a1", &admin)],
    )
    .unwrap();
    let before = chain.access_policy().clone();
    drop(chain);
    let reopened =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    assert_eq!(reopened.access_policy(), &before);
    assert_eq!(reopened.access_policy().mode, AccessMode::Allowlist);
    assert!(reopened.verify_signatures().is_clean());
    let _ = std::fs::remove_dir_all(dir);
}

/// Forged policy thoughts found in a chain (for example planted in the file) are ignored and reported.
#[test]
fn test_forged_policy_records_are_ignored_on_replay() {
    let (mut chain, dir) = open("forged", "c");
    let admin = secret(1);
    set_policy(
        &mut chain,
        "admin",
        "a1",
        &admin,
        AccessMode::Signed,
        &[("admin", "a1", &admin)],
    )
    .unwrap();
    let genuine = chain.access_policy().clone();
    // An attacker-built "open" record, unsigned, shaped like a real governance thought.
    let record = PolicyRecord::new(
        1,
        AccessMode::Open,
        vec![],
        genuine.head_record_hash.as_deref().unwrap(),
    )
    .unwrap();
    let mut forged = chain.thoughts().last().unwrap().clone();
    forged.index += 1;
    forged.hash = "f".repeat(64);
    forged.content = record.to_content();
    forged.thought_signature = None;
    let mut thoughts = chain.thoughts().to_vec();
    thoughts.push(forged);
    let replayed = AccessPolicy::replay("c", &thoughts);
    assert_eq!(
        replayed.mode,
        AccessMode::Signed,
        "the forged loosening must not apply"
    );
    assert_eq!(replayed.invalid_records, vec!["f".repeat(64)]);
    let _ = std::fs::remove_dir_all(dir);
}

/// Merkle commitment: the record's root equals the root over its sorted leaves and changes with the key set.
#[test]
fn test_policy_root_commits_to_the_key_set() {
    let (a, b) = (secret(1), secret(2));
    let one = PolicyRecord::new(
        0,
        AccessMode::Signed,
        vec![AllowedKey::new("x", "1", &public(&a))],
        "",
    )
    .unwrap();
    let two = PolicyRecord::new(
        0,
        AccessMode::Signed,
        vec![
            AllowedKey::new("x", "1", &public(&a)),
            AllowedKey::new("y", "2", &public(&b)),
        ],
        "",
    )
    .unwrap();
    assert_ne!(one.keys_root, two.keys_root);
    let swapped = PolicyRecord::new(
        0,
        AccessMode::Signed,
        vec![
            AllowedKey::new("y", "2", &public(&b)),
            AllowedKey::new("x", "1", &public(&a)),
        ],
        "",
    )
    .unwrap();
    assert_eq!(two, swapped, "input order must not matter");
    let leaves: Vec<[u8; 32]> = two.keys.iter().map(|k| k.leaf().unwrap()).collect();
    assert_eq!(
        two.keys_root,
        mentisdb::signing::to_hex(&mentisdb::signing::merkle_root(&leaves))
    );
    // Duplicate identities are refused.
    assert!(PolicyRecord::new(
        0,
        AccessMode::Signed,
        vec![
            AllowedKey::new("x", "1", &public(&a)),
            AllowedKey::new("x", "1", &public(&b))
        ],
        ""
    )
    .is_err());
}
