//! Tests for the shared Ed25519 primitives in `mentisdb::signing`: key
//! validation, verification, hex, signature versions, payload binding and the
//! key-set Merkle commitment.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::signing::*;
use mentisdb::{ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType};
use uuid::Uuid;

fn key(seed: u8) -> (SigningKey, [u8; 32]) {
    let secret = SigningKey::from_bytes(&[seed; 32]);
    let public = secret.verifying_key().to_bytes();
    (secret, public)
}

/// Honest signatures verify; tampering, a wrong key and bad lengths are refused.
#[test]
fn test_verify_accepts_valid_and_rejects_everything_else() {
    let (secret, public) = key(1);
    let sig = secret.sign(b"m").to_bytes();
    assert!(verify_ed25519_signature(&public, b"m", &sig).is_ok());
    assert_eq!(
        verify_ed25519_signature(&public, b"x", &sig),
        Err(SigningError::VerificationFailed)
    );
    assert_eq!(
        verify_ed25519_signature(&key(2).1, b"m", &sig),
        Err(SigningError::VerificationFailed)
    );
    assert_eq!(
        verify_ed25519_signature(&public, b"m", &sig[..10]),
        Err(SigningError::SignatureLength(10))
    );
    assert_eq!(
        verify_ed25519_signature(&public[..31], b"m", &sig),
        Err(SigningError::PublicKeyLength(31))
    );
}

/// Finding 1: only real, non-weak curve points are keys.
#[test]
fn test_public_key_must_be_a_strong_curve_point() {
    assert!(validate_public_key(&key(9).1).is_ok());
    assert_eq!(
        validate_public_key(&[]),
        Err(SigningError::PublicKeyLength(0))
    );
    assert_eq!(
        validate_public_key(&[0u8; 33]),
        Err(SigningError::PublicKeyLength(33))
    );
    let mut identity = [0u8; 32];
    identity[0] = 1;
    assert_eq!(
        validate_public_key(&identity),
        Err(SigningError::PublicKeyWeak)
    );
    // y = 2 is not the y-coordinate of any curve point.
    let mut off_curve = [0u8; 32];
    off_curve[0] = 2;
    assert_eq!(
        validate_public_key(&off_curve),
        Err(SigningError::PublicKeyNotOnCurve)
    );
}

/// Hex round-trips in either case and rejects malformed text.
#[test]
fn test_hex_round_trip_and_rejection() {
    let bytes: Vec<u8> = (0..=255).collect();
    assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
    assert_eq!(from_hex(" AbCd\n").unwrap(), vec![0xab, 0xcd]);
    for bad in ["", "a", "abc", "gg", "0x00"] {
        assert!(from_hex(bad).is_err(), "{bad:?}");
    }
}

/// 64 bytes is v1, `0x02` + 64 bytes is v2, everything else is refused.
#[test]
fn test_stored_signature_versions() {
    let sig = [5u8; 64];
    assert_eq!(
        StoredSignature::parse(&sig).unwrap().version,
        SignatureVersion::V1
    );
    let encoded = StoredSignature::encode_v2(&sig);
    assert_eq!(encoded.len(), 65);
    let parsed = StoredSignature::parse(&encoded).unwrap();
    assert_eq!(
        (parsed.version, parsed.signature),
        (SignatureVersion::V2, sig)
    );
    let mut wrong_tag = encoded.clone();
    wrong_tag[0] = 0x03;
    for bad in [&[][..], &sig[..63], &wrong_tag[..], &[0x02u8; 66][..]] {
        assert_eq!(
            StoredSignature::parse(bad),
            Err(SigningError::UnsupportedSignatureVersion)
        );
    }
}

/// Finding 2: every semantic field, chain and position change the v2 payload.
#[test]
fn test_thought_payload_v2_covers_every_field() {
    let base = ThoughtInput::new(ThoughtType::Decision, "ship");
    let reference = thought_payload_v2("c", "a", "h", &base);
    let other_id = Uuid::new_v4();
    let variants: Vec<(&str, ThoughtInput)> = vec![
        (
            "content",
            ThoughtInput {
                content: "other".into(),
                ..base.clone()
            },
        ),
        (
            "type",
            ThoughtInput {
                thought_type: ThoughtType::Insight,
                ..base.clone()
            },
        ),
        (
            "role",
            ThoughtInput {
                role: mentisdb::ThoughtRole::Audit,
                ..base.clone()
            },
        ),
        (
            "importance",
            ThoughtInput {
                importance: 0.9,
                ..base.clone()
            },
        ),
        (
            "confidence",
            ThoughtInput {
                confidence: Some(0.5),
                ..base.clone()
            },
        ),
        (
            "tags",
            ThoughtInput {
                tags: vec!["t".into()],
                ..base.clone()
            },
        ),
        (
            "concepts",
            ThoughtInput {
                concepts: vec!["k".into()],
                ..base.clone()
            },
        ),
        (
            "refs",
            ThoughtInput {
                refs: vec![1],
                ..base.clone()
            },
        ),
        (
            "entity_type",
            ThoughtInput {
                entity_type: Some("person".into()),
                ..base.clone()
            },
        ),
        (
            "source_episode",
            ThoughtInput {
                source_episode: Some("ep".into()),
                ..base.clone()
            },
        ),
        (
            "session",
            ThoughtInput {
                session_id: Some(other_id),
                ..base.clone()
            },
        ),
        (
            "scope",
            base.clone().with_scope(mentisdb::MemoryScope::User),
        ),
        (
            "relation",
            base.clone().with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::Corrects,
                target_id: other_id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        ),
        (
            "valid_at",
            base.clone().with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::Corrects,
                target_id: other_id,
                chain_key: None,
                valid_at: Some(chrono::Utc::now()),
                invalid_at: None,
            }]),
        ),
    ];
    for (name, input) in variants {
        assert_ne!(
            reference,
            thought_payload_v2("c", "a", "h", &input),
            "{name} is not covered"
        );
    }
    assert_ne!(
        reference,
        thought_payload_v2("c2", "a", "h", &base),
        "chain_key"
    );
    assert_ne!(
        reference,
        thought_payload_v2("c", "a2", "h", &base),
        "agent_id"
    );
    assert_ne!(
        reference,
        thought_payload_v2("c", "a", "h2", &base),
        "prev_hash"
    );
}

/// Normalization (trim, de-duplicate, clamp) cannot change what is signed.
#[test]
fn test_thought_payload_v2_is_normalization_stable() {
    let raw = ThoughtInput {
        tags: vec![" a ".into(), "A".into(), "".into()],
        importance: 7.0,
        ..ThoughtInput::new(ThoughtType::Insight, "x")
    };
    let clean = ThoughtInput {
        tags: vec!["a".into()],
        importance: 1.0,
        ..ThoughtInput::new(ThoughtType::Insight, "x")
    };
    assert_eq!(
        thought_payload_v2("c", "a", "", &raw),
        thought_payload_v2("c", "a", "", &clean)
    );
}

/// Domain separation: a registration proof is not a valid thought signature.
#[test]
fn test_payload_domains_are_distinct() {
    let (secret, public) = key(4);
    let reg = key_registration_payload("c", "a", "k", &public, 1);
    assert!(reg.starts_with(KEY_REGISTRATION_DOMAIN));
    let thought = thought_payload_v2("c", "a", "", &ThoughtInput::new(ThoughtType::Insight, "x"));
    assert!(thought.starts_with(THOUGHT_V2_DOMAIN));
    let proof = secret.sign(&reg).to_bytes();
    assert!(verify_ed25519_signature(&public, &thought, &proof).is_err());
    // Every field of the registration payload matters.
    for changed in [
        key_registration_payload("c2", "a", "k", &public, 1),
        key_registration_payload("c", "a2", "k", &public, 1),
        key_registration_payload("c", "a", "k2", &public, 1),
        key_registration_payload("c", "a", "k", &key(5).1, 1),
        key_registration_payload("c", "a", "k", &public, 2),
    ] {
        assert!(verify_ed25519_signature(&public, &changed, &proof).is_err());
    }
}

/// The Merkle root follows RFC 6962 shape: empty, single, order-sensitive, odd counts.
#[test]
fn test_merkle_root_shape() {
    let leaf = |i: u8| access_key_leaf("a", &format!("k{i}"), &[i; 32]);
    let sha_empty = {
        use sha2::{Digest, Sha256};
        let d: [u8; 32] = Sha256::digest([]).into();
        d
    };
    assert_eq!(merkle_root(&[]), sha_empty);
    assert_eq!(merkle_root(&[leaf(1)]), leaf(1));
    assert_ne!(
        merkle_root(&[leaf(1), leaf(2)]),
        merkle_root(&[leaf(2), leaf(1)])
    );
    let three = merkle_root(&[leaf(1), leaf(2), leaf(3)]);
    assert_ne!(three, merkle_root(&[leaf(1), leaf(2)]));
    // n = 3 splits 2 + 1.
    let expect = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update([1u8]);
        h.update(merkle_root(&[leaf(1), leaf(2)]));
        h.update(leaf(3));
        let d: [u8; 32] = h.finalize().into();
        d
    };
    assert_eq!(three, expect);
    // n = 5 splits 4 + 1 and never panics for larger sets.
    let many: Vec<_> = (0..40).map(leaf).collect();
    for n in 0..many.len() {
        let _ = merkle_root(&many[..n]);
    }
}
