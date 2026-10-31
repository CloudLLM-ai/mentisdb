//! Regression tests for agent key registration: valid curve points only, and a
//! fresh proof of possession bound to chain, agent, key id and key.

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{AccessError, KeyRegistrationProof};
use mentisdb::signing::key_registration_payload;
use mentisdb::{MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtType};

fn chain(name: &str) -> (MentisDb, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mentisdb_keyreg_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain
        .append("alice", ThoughtType::Insight, "hello")
        .unwrap();
    (chain, dir)
}

fn proof_for(
    secret: &SigningKey,
    chain: &str,
    agent: &str,
    key_id: &str,
    at: i64,
) -> KeyRegistrationProof {
    let public = secret.verifying_key().to_bytes();
    let payload = key_registration_payload(chain, agent, key_id, &public, at);
    KeyRegistrationProof {
        registered_at_unix_secs: at,
        signature: secret.sign(&payload).to_bytes().to_vec(),
    }
}

fn access_error(error: &std::io::Error) -> AccessError {
    AccessError::from_io(error)
        .expect("typed AccessError")
        .clone()
}

/// A key with a valid proof is registered and stored.
#[test]
fn test_valid_key_with_proof_is_registered() {
    let (mut chain, dir) = chain("ok");
    let secret = SigningKey::from_bytes(&[1; 32]);
    let record = chain
        .add_agent_key(
            "alice",
            "k1",
            PublicKeyAlgorithm::Ed25519,
            secret.verifying_key().to_bytes().to_vec(),
            &proof_for(&secret, "reg", "alice", "k1", Utc::now().timestamp()),
        )
        .unwrap();
    assert_eq!(record.public_keys[0].key_id, "k1");
    let _ = std::fs::remove_dir_all(dir);
}

/// Finding: a key of the wrong size, off the curve, or a small-order point is refused,
/// even with a (necessarily forged) proof.
#[test]
fn test_malformed_and_weak_keys_are_rejected() {
    let (mut chain, dir) = chain("bad");
    let forged = KeyRegistrationProof {
        registered_at_unix_secs: Utc::now().timestamp(),
        signature: vec![0; 64],
    };
    let mut identity = vec![0u8; 32];
    identity[0] = 1;
    let mut off_curve = vec![0u8; 32];
    off_curve[0] = 2;
    for key in [vec![1, 2, 3, 4], vec![], identity, off_curve] {
        let error = chain
            .add_agent_key("alice", "k", PublicKeyAlgorithm::Ed25519, key, &forged)
            .unwrap_err();
        assert!(
            matches!(access_error(&error), AccessError::InvalidPublicKey(_)),
            "{error}"
        );
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(chain
        .agent_registry()
        .agents
        .values()
        .all(|a| a.public_keys.is_empty()));
    let _ = std::fs::remove_dir_all(dir);
}

/// Finding: registering someone else's public key without their private key fails.
#[test]
fn test_proof_of_possession_is_required() {
    let (mut chain, dir) = chain("pop");
    let victim = SigningKey::from_bytes(&[2; 32]);
    let attacker = SigningKey::from_bytes(&[3; 32]);
    let now = Utc::now().timestamp();
    let public = victim.verifying_key().to_bytes().to_vec();
    let forged = proof_for(&attacker, "reg", "alice", "k", now);
    let error = chain
        .add_agent_key("alice", "k", PublicKeyAlgorithm::Ed25519, public, &forged)
        .unwrap_err();
    assert!(matches!(
        access_error(&error),
        AccessError::KeyProofInvalid(_)
    ));
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let _ = std::fs::remove_dir_all(dir);
}

/// A proof is bound to its chain, agent, key id and time, so it cannot be replayed elsewhere.
#[test]
fn test_proof_cannot_be_replayed_for_other_identities_or_times() {
    let (mut chain, dir) = chain("replay");
    chain.append("bob", ThoughtType::Insight, "hi").unwrap();
    let secret = SigningKey::from_bytes(&[4; 32]);
    let public = secret.verifying_key().to_bytes().to_vec();
    let now = Utc::now().timestamp();
    let ok = proof_for(&secret, "reg", "alice", "k1", now);
    // Other agent, other key id.
    for (agent, key_id) in [("bob", "k1"), ("alice", "k2")] {
        let error = chain
            .add_agent_key(
                agent,
                key_id,
                PublicKeyAlgorithm::Ed25519,
                public.clone(),
                &ok,
            )
            .unwrap_err();
        assert!(
            matches!(access_error(&error), AccessError::KeyProofInvalid(_)),
            "{agent}/{key_id}"
        );
    }
    // Proof made for another chain.
    let other_chain = proof_for(&secret, "elsewhere", "alice", "k1", now);
    assert!(chain
        .add_agent_key(
            "alice",
            "k1",
            PublicKeyAlgorithm::Ed25519,
            public.clone(),
            &other_chain
        )
        .is_err());
    // Stale proof, correctly signed for its own (old) timestamp.
    let stale = proof_for(&secret, "reg", "alice", "k1", now - 3_600);
    let error = chain
        .add_agent_key(
            "alice",
            "k1",
            PublicKeyAlgorithm::Ed25519,
            public.clone(),
            &stale,
        )
        .unwrap_err();
    assert!(matches!(
        access_error(&error),
        AccessError::KeyProofStale { .. }
    ));
    // Far-future timestamps are refused too.
    let future = proof_for(&secret, "reg", "alice", "k1", now + 3_600);
    assert!(chain
        .add_agent_key("alice", "k1", PublicKeyAlgorithm::Ed25519, public, &future)
        .is_err());
    let _ = std::fs::remove_dir_all(dir);
}
