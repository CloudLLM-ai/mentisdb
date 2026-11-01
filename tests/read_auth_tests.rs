//! Read proofs: binding, freshness, replay and a strict header grammar.

use mentisdb::read_auth::{
    fresh_nonce, sign_request, verify_request, NonceCache, ReadAuthError, ReadAuthHeader,
    MAX_READ_AUTH_HEADER_LEN, READ_AUTH_WINDOW_SECS,
};

const SEED: [u8; 32] = [11; 32];
const NOW: i64 = 1_767_225_600;
const NONCE: &str = "00112233445566778899aabbccddeeff";

fn header(body: &[u8]) -> String {
    sign_request(
        &SEED,
        "alice",
        "alice-1",
        "POST",
        "/v1/search?x=1",
        body,
        NOW,
        NONCE,
    )
}

#[test]
fn a_fresh_proof_verifies_and_names_the_signer() {
    let proof = verify_request(&header(b"{}"), "POST", "/v1/search?x=1", b"{}", NOW).unwrap();
    assert_eq!(
        (proof.agent_id.as_str(), proof.key_id.as_str()),
        ("alice", "alice-1")
    );
    assert_eq!(proof.nonce, NONCE);
}

#[test]
fn method_case_does_not_matter_but_everything_else_is_bound() {
    let h = header(b"{}");
    assert!(verify_request(&h, "post", "/v1/search?x=1", b"{}", NOW).is_ok());
    for (method, target, body) in [
        ("GET", "/v1/search?x=1", &b"{}"[..]),
        ("POST", "/v1/search?x=2", &b"{}"[..]),
        ("POST", "/v1/search", &b"{}"[..]),
        ("POST", "/v1/search?x=1", &b"{ }"[..]),
        ("POST", "/v1/search?x=1", &b""[..]),
    ] {
        assert_eq!(
            verify_request(&h, method, target, body, NOW),
            Err(ReadAuthError::BadSignature),
            "{method} {target}"
        );
    }
}

#[test]
fn freshness_window_applies_in_both_directions() {
    let h = header(b"{}");
    for delta in [0, READ_AUTH_WINDOW_SECS, -READ_AUTH_WINDOW_SECS] {
        assert!(verify_request(&h, "POST", "/v1/search?x=1", b"{}", NOW + delta).is_ok());
    }
    for delta in [
        READ_AUTH_WINDOW_SECS + 1,
        -READ_AUTH_WINDOW_SECS - 1,
        86_400,
        i64::MAX / 2,
    ] {
        assert!(matches!(
            verify_request(&h, "POST", "/v1/search?x=1", b"{}", NOW + delta),
            Err(ReadAuthError::Stale { .. })
        ));
    }
    // Extreme clocks must not overflow.
    assert!(verify_request(&h, "POST", "/v1/search?x=1", b"{}", i64::MIN).is_err());
}

#[test]
fn tampering_with_any_header_field_breaks_the_proof() {
    let h = header(b"{}");
    let parsed = ReadAuthHeader::parse(&h).unwrap();
    let variants = [
        ReadAuthHeader {
            agent_id: "bob".into(),
            ..parsed.clone()
        },
        ReadAuthHeader {
            key_id: "alice-2".into(),
            ..parsed.clone()
        },
        ReadAuthHeader {
            timestamp: parsed.timestamp + 1,
            ..parsed.clone()
        },
        ReadAuthHeader {
            nonce: "ff".repeat(16),
            ..parsed.clone()
        },
        ReadAuthHeader {
            public_key: [1; 32],
            ..parsed.clone()
        },
        ReadAuthHeader {
            signature: [0; 64],
            ..parsed.clone()
        },
    ];
    for variant in variants {
        assert_eq!(
            verify_request(
                &variant.to_header_value(),
                "POST",
                "/v1/search?x=1",
                b"{}",
                NOW
            ),
            Err(ReadAuthError::BadSignature),
            "{variant:?}"
        );
    }
}

#[test]
fn weak_public_keys_never_verify() {
    // The identity point is a small-order key; strict verification rejects it.
    let mut parsed = ReadAuthHeader::parse(&header(b"{}")).unwrap();
    parsed.public_key = [0; 32];
    parsed.public_key[0] = 1;
    parsed.signature = [0; 64];
    assert!(verify_request(
        &parsed.to_header_value(),
        "POST",
        "/v1/search?x=1",
        b"{}",
        NOW
    )
    .is_err());
}

#[test]
fn a_proof_works_once() {
    let cache = NonceCache::new(4);
    let proof = verify_request(&header(b"{}"), "POST", "/v1/search?x=1", b"{}", NOW).unwrap();
    assert!(cache
        .check_and_insert(&proof.public_key, &proof.nonce, NOW)
        .is_ok());
    assert_eq!(
        cache.check_and_insert(&proof.public_key, &proof.nonce, NOW + 30),
        Err(ReadAuthError::Replayed)
    );
    // After the entry expires the cache would accept the nonce again, but the
    // timestamp window has closed by then, so the proof is stale anyway.
    let later = NOW + 2 * READ_AUTH_WINDOW_SECS + 1;
    assert!(cache
        .check_and_insert(&proof.public_key, &proof.nonce, later)
        .is_ok());
    assert!(matches!(
        verify_request(&header(b"{}"), "POST", "/v1/search?x=1", b"{}", later),
        Err(ReadAuthError::Stale { .. })
    ));
}

#[test]
fn the_nonce_cache_is_bounded_and_prunes_expired_entries() {
    let cache = NonceCache::new(3);
    for i in 0..3u8 {
        cache.check_and_insert(&[i; 32], NONCE, NOW).unwrap();
    }
    assert_eq!(
        cache.check_and_insert(&[9; 32], NONCE, NOW),
        Err(ReadAuthError::CacheFull)
    );
    assert_eq!(cache.len(), 3);
    // Once the entries expire there is room again.
    assert!(cache
        .check_and_insert(&[9; 32], NONCE, NOW + 2 * READ_AUTH_WINDOW_SECS + 1)
        .is_ok());
    assert!(cache.len() <= 3);
    assert!(!cache.is_empty());
}

#[test]
fn ids_with_awkward_characters_round_trip() {
    let h = sign_request(
        &SEED,
        "al ice;=%é/",
        "k 1",
        "GET",
        "/v1/chains",
        b"",
        NOW,
        &fresh_nonce(),
    );
    let proof = verify_request(&h, "GET", "/v1/chains", b"", NOW).unwrap();
    assert_eq!(proof.agent_id, "al ice;=%é/");
    assert_eq!(proof.key_id, "k 1");
    assert!(h.is_ascii() && !h.contains(['\n', '\r']));
}

#[test]
fn the_grammar_is_strict() {
    let good = header(b"{}");
    assert!(ReadAuthHeader::parse(&good).is_ok());
    let bad = [
        String::new(),
        "v1".to_string(),
        good.replacen("v1 ", "v2 ", 1),
        good.replacen("v1 ", "v1  ", 1),
        format!("{good};extra=1"),
        format!("{good};"),
        good.replacen("agent=alice;key=alice-1", "key=alice-1;agent=alice", 1),
        good.replacen("agent=alice", "agent=", 1),
        good.replacen("agent=alice", "agent=al%69ce", 1), // lowercase-hex-free but non-canonical escape of an unreserved byte
        good.replacen("agent=alice", "agent=al%6", 1),
        good.replacen("agent=alice", "agent=al%zz", 1),
        good.replacen("agent=alice", "agent=al ice", 1),
        good.replacen("ts=1767225600", "ts=+1767225600", 1),
        good.replacen("ts=1767225600", "ts=01767225600", 1),
        good.replacen("ts=1767225600", "ts=", 1),
        good.replacen(NONCE, &NONCE.to_uppercase(), 1),
        good.replacen(NONCE, "abc", 1),
        good.replacen(NONCE, &"a".repeat(65), 1),
        "x".repeat(MAX_READ_AUTH_HEADER_LEN + 1),
        good.replace("pk=", "pk=00"),
    ];
    for text in &bad {
        assert!(ReadAuthHeader::parse(text).is_err(), "accepted: {text}");
    }
    // Uppercase hex in the signature is rejected too.
    let parsed = ReadAuthHeader::parse(&good).unwrap();
    let upper = good.replace(
        &mentisdb::signing::to_hex(&parsed.signature),
        &mentisdb::signing::to_hex(&parsed.signature).to_uppercase(),
    );
    assert!(ReadAuthHeader::parse(&upper).is_err() || upper == good);
}

#[test]
fn overlong_ids_are_rejected_before_any_crypto() {
    let long = "a".repeat(257);
    let h = sign_request(&SEED, &long, "k", "GET", "/", b"", NOW, NONCE);
    assert!(matches!(
        verify_request(&h, "GET", "/", b"", NOW),
        Err(ReadAuthError::Malformed(_))
    ));
}

/// The preimage starts with its own domain tag, distinct from every other
/// MentisDB signature domain, so one signature cannot be replayed as another.
#[test]
fn domain_separation_from_other_signatures() {
    use mentisdb::read_auth::{read_auth_payload, READ_AUTH_DOMAIN};
    use mentisdb::signing::{ACCESS_KEY_LEAF_DOMAIN, KEY_REGISTRATION_DOMAIN, THOUGHT_V2_DOMAIN};
    let parsed = ReadAuthHeader::parse(&header(b"{}")).unwrap();
    let payload = read_auth_payload("POST", "/", b"", &parsed);
    assert!(payload.starts_with(READ_AUTH_DOMAIN));
    for other in [
        THOUGHT_V2_DOMAIN,
        KEY_REGISTRATION_DOMAIN,
        ACCESS_KEY_LEAF_DOMAIN,
    ] {
        assert!(!payload.starts_with(other));
        assert!(!READ_AUTH_DOMAIN.starts_with(other) && !other.starts_with(READ_AUTH_DOMAIN));
    }
}
