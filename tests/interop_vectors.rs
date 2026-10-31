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

/// `(op, payload hex, signature hex)` pinned by mentischat for room `den`, signer
/// `alice`, counter 7 (copied from its `tests/interop_vectors.rs`).
const MENTISCHAT_PINNED: [(&str, &str, &str); 9] = [
    ("register_key", "6d656e746973636861742f72656769737465722d6b65792f7631000000000000000005616c696365000000000000000079b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad049664", "cc5a84e97bf94a431d2e50d10050e669805e46667321f2f716b3f2399af6ddbee8d51bac756f222ee13dc3e887793c9734513c7bd1832fa47587f297d720b009"),
    ("create", "6d656e746973636861742f6372656174652f7631000000000364656e00000005616c6963650000000000000007000000077468652064656e", "b321fdf1d6fe8bdd24d5d2c50179a07e3a94b0633d9225315d0cf3ab229e032c33fc2f72fc9a8568dd919069b9853272cc666a49e504be4bf009c5d2411c6a00"),
    ("invite", "6d656e746973636861742f696e766974652f7631000000000364656e00000005616c696365000000000000000700000003626f628139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394", "eb0bf1d4cb4a6fc45573a035ac580fec975fbc431d9cc0cb9a0ae134ef054860dd138cab6925d06169ab50cff994578f47a01678bb0da78c327d52f804755006"),
    ("join", "6d656e746973636861742f6a6f696e2f7631000000000364656e00000005616c6963650000000000000007", "3a22cd642bef0d80bf4cff234675df8383361e99af0d91da2af370bc42214e195158f79f17d457e24f5012a6c9764b7ec3e667ba87d6babf0b446176ed86f206"),
    ("leave", "6d656e746973636861742f6c656176652f7631000000000364656e00000005616c6963650000000000000007", "d5533b9d2a715dbc327021070654280d2f1a375b5fedb2c92b56e1bae5e0c43084c72666b77fd9bbd844a3cf3648dee39cb97f2bdacee181a1ad03fe1d3c590d"),
    ("remove", "6d656e746973636861742f72656d6f76652f7631000000000364656e00000005616c696365000000000000000700000003626f62", "f78f80d00a951517f2b3a9df3b73d1f9792c742813ea9d5b5f6731b9bf71f365133d3753aa9dcc5002442081588af5ddc82ac98d97d42693286cef32c615d500"),
    ("send", "6d656e746973636861742f73656e642f7631000000000364656e00000005616c6963650000000000000007000000164e6f207265706c79206e65656465643a2068656c6c6f", "23f31b594bbd815b2341d989a865b11117fc205888d33379a0caeea7cf9f2c3e94d7731ddab81dac36f33de4b06662d1e372ebe864b9023aef20ea58ef009802"),
    ("welcome", "6d656e746973636861742f77656c636f6d652f7631000000000364656e00000005616c696365000000000000000700000003626f6200000003010203", "07d5f256b2919994621e85da9a7ee7f9f7b34565ac350c3147912ccd49ab07e725877a292780a3f3d86a38e0d1d62af43094db0a611fc0b75d9fc773ec1c8f02"),
    ("key_packages", "6d656e746973636861742f6b65792d7061636b616765732f7631000000000000000005616c6963650000000000000007000000020000000201020000000103010000000109", "255d0823aa9fe5fa1c06fef6b666c6e7bc0f9c24570833376929e34f4c8939a5608d89a5526d9b4914abbd533d2a523ca00e1c74559283802e4a329b33153d08"),
];

/// mentischat's signatures verify with MentisDB's strict verifier over the raw
/// key and raw payload, fail for a changed payload, and never share a domain tag
/// with MentisDB's own payloads.
#[test]
fn test_mentischat_signatures_verify_with_the_mentisdb_verifier() {
    let public = key().verifying_key().to_bytes();
    for (op, payload_hex, signature_hex) in MENTISCHAT_PINNED {
        let payload = mentisdb::signing::from_hex(payload_hex).unwrap();
        let signature = mentisdb::signing::from_hex(signature_hex).unwrap();
        assert!(
            verify_ed25519_signature(&public, &payload, &signature).is_ok(),
            "{op}"
        );
        let mut changed = payload.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(
            verify_ed25519_signature(&public, &changed, &signature).is_err(),
            "{op} changed"
        );
        assert!(payload.starts_with(b"mentischat/"), "{op}");
        assert!(!payload.starts_with(b"mentisdb/"), "{op}");
    }
}
