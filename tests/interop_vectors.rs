//! Cross-project Ed25519 vectors shared with mentischat.
//!
//! The seed is the 32 bytes 0x01..=0x20. If the public key hex below ever
//! differs between this file and mentischat's copy, one agent identity no longer
//! works in both systems. Ed25519 is deterministic, so the signatures are fixed
//! too; they pin each MentisDB payload encoding byte for byte (see
//! `docs/CHAIN_ACCESS.md`), and any implementation of those encodings must
//! reproduce them.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{policy_thought_input, AccessMode, AllowedKey, PolicyRecord};
use mentisdb::signing::{
    access_key_leaf, key_registration_payload, merkle_root, thought_payload_v2, to_hex,
    verify_ed25519_signature,
};
use mentisdb::{ThoughtInput, ThoughtType};

/// The seed 0x01..=0x20.
fn seed() -> [u8; 32] {
    std::array::from_fn(|i| i as u8 + 1)
}

const PUBLIC_KEY_HEX: &str = "79b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad049664";
const THOUGHT_V2_SIGNATURE_HEX: &str = "cc23afe8ff51d55fcfab64a166425fcbfe12eded2f19286cc6334e765e59cd269f34b3278ee0863f6879a1d7246949ab96542c7753f59722328e70165d06cd03";
const KEY_REGISTRATION_SIGNATURE_HEX: &str = "6e5e11e8b2c5b22ec5e4c6b72df524fcacc05dc0f0f2925e989359a6f09be87bec681f7afdaade563e661f05d1fd2fb4a8e997020a8734d0e853454f6b314509";
const POLICY_RECORD_SIGNATURE_HEX: &str = "a29dee7a27ddd03dc5ea2646258573e4d02fc405e9908539d7e7f1bdcf36a230321fba33b0dee1e9cf6ff3c8ae3f3bf609858e9a92fdf03248ddf101381ffa0e";
const KEYS_ROOT_HEX: &str = "ad4c145352c5650011e6e69a77e7af5c5ea925ae514a06bbdab69a740aaecef2";

fn key() -> SigningKey {
    SigningKey::from_bytes(&seed())
}

fn fixed_thought() -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Decision, "interop vector")
}

fn fixed_record() -> PolicyRecord {
    let public = key().verifying_key().to_bytes();
    PolicyRecord::new(
        0,
        AccessMode::Allowlist,
        vec![AllowedKey::new("alice", "alice-202610", &public)],
        "",
    )
    .unwrap()
}

/// The shared identity: seed 0x01..0x20 has this public key.
#[test]
fn test_public_key_is_the_shared_identity() {
    assert_eq!(to_hex(&key().verifying_key().to_bytes()), PUBLIC_KEY_HEX);
}

/// Each payload type signs to a fixed signature that verifies under the shared key.
#[test]
fn test_signatures_over_fixed_payloads() {
    let public = key().verifying_key().to_bytes();
    let thought = thought_payload_v2("interop", "alice", "", &fixed_thought());
    let registration =
        key_registration_payload("interop", "alice", "alice-202610", &public, 1_790_000_000);
    let policy = thought_payload_v2(
        "interop",
        "alice",
        "",
        &policy_thought_input(&fixed_record()),
    );
    for (name, payload, expected) in [
        ("thought v2", thought, THOUGHT_V2_SIGNATURE_HEX),
        (
            "key registration",
            registration,
            KEY_REGISTRATION_SIGNATURE_HEX,
        ),
        ("policy record", policy, POLICY_RECORD_SIGNATURE_HEX),
    ] {
        let signature = key().sign(&payload).to_bytes();
        assert_eq!(to_hex(&signature), expected, "{name}");
        assert!(
            verify_ed25519_signature(&public, &payload, &signature).is_ok(),
            "{name}"
        );
    }
}

/// The Merkle root of the one-key set pins the leaf encoding and tree hash.
#[test]
fn test_keys_root_of_the_shared_identity() {
    let public = key().verifying_key().to_bytes();
    let leaf = access_key_leaf("alice", "alice-202610", &public);
    assert_eq!(to_hex(&merkle_root(&[leaf])), KEYS_ROOT_HEX);
    assert_eq!(fixed_record().keys_root, KEYS_ROOT_HEX);
}
