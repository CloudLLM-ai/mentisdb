//! Pins SHA-256 digests and Ed25519 signatures to published known-answer
//! vectors so dependency upgrades (sha2, ed25519-dalek) cannot silently
//! change persisted chain hashes, sidecar digests, or signature validity.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// FIPS 180-2 vector: SHA-256("abc"), through both one-shot and incremental
/// APIs and through the `{:x}` formatting used by backup manifests.
#[test]
fn test_sha256_known_answer_is_stable() {
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(hex(&Sha256::digest(b"abc")), expected);
    assert_eq!(
        Sha256::digest(b"abc")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        expected
    );
    let mut hasher = Sha256::new();
    hasher.update(b"a");
    hasher.update(b"bc");
    assert_eq!(hex(&hasher.finalize()), expected);
}

/// RFC 8032 section 7.1 TEST 1: derived public key, signature bytes, and
/// verification must match exactly, including a pre-existing signature.
#[test]
fn test_ed25519_rfc8032_vector_signs_and_verifies() {
    let secret: [u8; 32] =
        unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .try_into()
            .unwrap();
    let public = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    let sig_hex = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
                   5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    let signing = SigningKey::from_bytes(&secret);
    assert_eq!(hex(signing.verifying_key().as_bytes()), public);
    assert_eq!(hex(&signing.sign(b"").to_bytes()), sig_hex);

    let vk = VerifyingKey::from_bytes(&unhex(public).try_into().unwrap()).unwrap();
    let sig = Signature::from_bytes(&unhex(sig_hex).try_into().unwrap());
    assert!(vk.verify(b"", &sig).is_ok());
    assert!(vk.verify(b"tampered", &sig).is_err());
}
