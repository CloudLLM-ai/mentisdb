//! Governance and signature hardening: Merkle-root replay, v1 downgrade, the
//! stale-head race between signers and revocation of the last governor's key.

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessError, AccessMode, AccessPolicy, AllowedKey, KeyRegistrationProof,
    PolicyRecord,
};
use mentisdb::signing::{
    key_registration_payload, thought_payload_v2, thought_payload_v2_stored, StoredSignature,
};
use mentisdb::{
    signable_thought_payload, MentisDb, PublicKeyAlgorithm, StorageAdapterKind, Thought,
    ThoughtInput, ThoughtRole, ThoughtType,
};

fn open(tag: &str) -> (MentisDb, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mentisdb_sec_gov_{tag}_{}", uuid::Uuid::new_v4()));
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "room", StorageAdapterKind::Binary).unwrap();
    (chain, dir)
}

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}
fn pk(key: &SigningKey) -> Vec<u8> {
    key.verifying_key().to_bytes().to_vec()
}
fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

fn register(chain: &mut MentisDb, agent: &str, key_id: &str, key: &SigningKey) {
    if !chain.agent_registry().agents.contains_key(agent) {
        chain
            .upsert_agent(agent, Some(agent), None, None, None)
            .unwrap();
    }
    let now = Utc::now().timestamp();
    let payload = key_registration_payload(chain.chain_key(), agent, key_id, &pk(key), now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key.sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(agent, key_id, PublicKeyAlgorithm::Ed25519, pk(key), &proof)
        .unwrap();
}

fn sign_v2(
    chain: &MentisDb,
    agent: &str,
    key_id: &str,
    key: &SigningKey,
    text: &str,
) -> ThoughtInput {
    let mut input = ThoughtInput::new(ThoughtType::Insight, text);
    let payload = thought_payload_v2(chain.chain_key(), agent, &head(chain), &input);
    input.signing_key_id = Some(key_id.into());
    input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
    input
}

fn allowlist(chain: &mut MentisDb, agent: &str, key_id: &str, key: &SigningKey) {
    let record = PolicyRecord::new(
        0,
        AccessMode::Allowlist,
        vec![AllowedKey::new(agent, key_id, &pk(key))],
        "",
    )
    .unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        agent,
        &head(chain),
        &policy_thought_input(&record),
    );
    let sig = StoredSignature::encode_v2(&key.sign(&payload).to_bytes());
    chain
        .set_access_policy(agent, &record, key_id, sig)
        .unwrap();
}

fn err(result: std::io::Result<&Thought>) -> AccessError {
    AccessError::from_io(&result.expect_err("must be refused"))
        .unwrap()
        .clone()
}

/// (1) A record whose `keys_root` does not commit to its keys is rejected on
/// replay even when a governor signed it; the chain keeps its previous policy
/// and reports the record as invalid.
#[test]
fn replay_recomputes_and_compares_the_merkle_root() {
    let (mut chain, dir) = open("root");
    let gov = sk(1);
    register(&mut chain, "gov", "g1", &gov);
    allowlist(&mut chain, "gov", "g1", &gov);

    // Forge seq 1 with a root that omits the extra key, signed by the governor.
    let extra = sk(2);
    let honest = PolicyRecord::new(
        1,
        AccessMode::Allowlist,
        vec![
            AllowedKey::new("gov", "g1", &pk(&gov)),
            AllowedKey::new("evil", "e1", &pk(&extra)),
        ],
        chain.access_policy().head_record_hash.as_deref().unwrap(),
    )
    .unwrap();
    let mut forged = honest.clone();
    forged.keys_root = honest
        .keys
        .iter()
        .map(|_| "00")
        .collect::<String>()
        .repeat(32)[..64]
        .to_string();
    // The library refuses to commit it...
    let payload = thought_payload_v2(
        chain.chain_key(),
        "gov",
        &head(&chain),
        &policy_thought_input(&forged),
    );
    let sig = StoredSignature::encode_v2(&gov.sign(&payload).to_bytes());
    assert!(matches!(
        AccessError::from_io(
            &chain
                .set_access_policy("gov", &forged, "g1", sig.clone())
                .unwrap_err()
        ),
        Some(AccessError::PolicyInvalid(_))
    ));
    // ...and replay of a chain that somehow holds it rejects it too.
    let mut thoughts: Vec<Thought> = chain.thoughts().to_vec();
    let mut raw = thoughts.last().unwrap().clone();
    raw.thought_type = ThoughtType::AccessPolicy;
    raw.role = ThoughtRole::Audit;
    raw.content = forged.to_content();
    raw.agent_id = "gov".into();
    raw.signing_key_id = Some("g1".into());
    raw.prev_hash = head(&chain);
    raw.hash = "forged-hash".into();
    raw.thought_signature = None;
    let payload = thought_payload_v2_stored("room", &raw);
    raw.thought_signature = Some(StoredSignature::encode_v2(&gov.sign(&payload).to_bytes()));
    thoughts.push(raw);
    let replayed = AccessPolicy::replay("room", &thoughts);
    assert_eq!(replayed.invalid_records, vec!["forged-hash".to_string()]);
    assert_eq!(replayed.seq, Some(0), "previous policy stays in force");
    assert_eq!(replayed.keys.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// (2) A v1-signed thought from an allowed key is refused on signed and
/// allowlist chains, and nothing is written.
#[test]
fn v1_signature_from_an_allowed_key_is_refused_on_restricted_chains() {
    let (mut chain, dir) = open("v1");
    let key = sk(3);
    register(&mut chain, "w", "w1", &key);
    allowlist(&mut chain, "w", "w1", &key);
    let before = chain.thoughts().len();
    let mut v1 = ThoughtInput::new(ThoughtType::Insight, "downgrade");
    let payload = signable_thought_payload("w", &v1);
    v1.signing_key_id = Some("w1".into());
    v1.thought_signature = Some(key.sign(&payload).to_bytes().to_vec());
    assert!(matches!(
        err(chain.append_thought("w", v1)),
        AccessError::SignatureVersionNotAllowed(_)
    ));
    assert_eq!(chain.thoughts().len(), before);
    let _ = std::fs::remove_dir_all(dir);
}

/// (3) Two signers build on the same head: exactly one lands, the other gets
/// the typed stale-head error and nothing partial is written; re-signing works.
#[test]
fn second_signer_on_the_same_head_gets_stale_head() {
    for governed in [false, true] {
        let (mut chain, dir) = open("stale");
        let (a, b) = (sk(4), sk(5));
        register(&mut chain, "a", "a1", &a);
        register(&mut chain, "b", "b1", &b);
        if governed {
            let record = PolicyRecord::new(
                0,
                AccessMode::Allowlist,
                vec![
                    AllowedKey::new("a", "a1", &pk(&a)),
                    AllowedKey::new("b", "b1", &pk(&b)),
                ],
                "",
            )
            .unwrap();
            let payload =
                thought_payload_v2("room", "a", &head(&chain), &policy_thought_input(&record));
            let sig = StoredSignature::encode_v2(&a.sign(&payload).to_bytes());
            chain.set_access_policy("a", &record, "a1", sig).unwrap();
        }
        let from_a = sign_v2(&chain, "a", "a1", &a, "from a");
        let from_b = sign_v2(&chain, "b", "b1", &b, "from b");
        let len = chain.thoughts().len();
        chain.append_thought("a", from_a).unwrap();
        assert_eq!(
            err(chain.append_thought("b", from_b)),
            AccessError::StaleHead
        );
        assert_eq!(chain.thoughts().len(), len + 1, "nothing partial");
        // Re-signing against the new head succeeds.
        let retry = sign_v2(&chain, "b", "b1", &b, "from b again");
        chain.append_thought("b", retry).unwrap();
        // A forged signature is still plain BadSignature, not StaleHead.
        let mut forged = sign_v2(&chain, "b", "b1", &b, "x");
        forged.content = "tampered".into();
        assert!(matches!(
            err(chain.append_thought("b", forged)),
            AccessError::BadSignature(_)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// (5) Revoking the governor's registry key does not lock the policy: governors
/// are the policy's own keys, so the last governor can still rotate.
#[test]
fn revoking_the_last_governors_registry_key_does_not_lock_governance() {
    let (mut chain, dir) = open("lockout");
    let gov = sk(6);
    register(&mut chain, "gov", "g1", &gov);
    allowlist(&mut chain, "gov", "g1", &gov);
    chain.revoke_agent_key("gov", "g1").unwrap();
    // The revoked key can no longer write ordinary thoughts...
    let blocked = sign_v2(&chain, "gov", "g1", &gov, "note");
    assert!(matches!(
        err(chain.append_thought("gov", blocked)),
        AccessError::KeyRevoked { .. }
    ));
    // ...but governance still works: rotate to a fresh key.
    let fresh = sk(7);
    let record = PolicyRecord::new(
        1,
        AccessMode::Allowlist,
        vec![AllowedKey::new("gov", "g2", &pk(&fresh))],
        chain.access_policy().head_record_hash.as_deref().unwrap(),
    )
    .unwrap();
    let payload = thought_payload_v2("room", "gov", &head(&chain), &policy_thought_input(&record));
    let sig = StoredSignature::encode_v2(&gov.sign(&payload).to_bytes());
    chain.set_access_policy("gov", &record, "g1", sig).unwrap();
    assert_eq!(chain.access_policy().seq, Some(1));
    let _ = std::fs::remove_dir_all(dir);
}
