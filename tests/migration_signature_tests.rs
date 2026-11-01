//! Signatures must survive a migration, and a migration that would break one
//! must be refused rather than silently invalidate it.

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::KeyRegistrationProof;
use mentisdb::migration::fixtures::{
    semantic_chain, write_fixture, write_schema3_base_as_is, FixtureFormat,
};
use mentisdb::migration::{
    run_chain_migration, MigrationError, MigrationOptions, MigrationOutcome,
};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{
    chain_storage_filename, MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput,
    ThoughtType,
};
use tempfile::TempDir;

const KEY: &str = "signed";

/// Build a chain with real v2 signatures through the library.
fn signed_chain(dir: &std::path::Path) {
    let key = SigningKey::from_bytes(&[5; 32]);
    let public = key.verifying_key().to_bytes().to_vec();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, KEY, StorageAdapterKind::Binary).unwrap();
    chain
        .upsert_agent("alice", Some("alice"), None, None, None)
        .unwrap();
    let now = Utc::now().timestamp();
    let payload = key_registration_payload(KEY, "alice", "k1", &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key.sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key("alice", "k1", PublicKeyAlgorithm::Ed25519, public, &proof)
        .unwrap();
    for n in 0..4 {
        let head = chain
            .thoughts()
            .last()
            .map(|t| t.hash.clone())
            .unwrap_or_default();
        let mut input = ThoughtInput::new(ThoughtType::Insight, format!("signed thought {n}"));
        let payload = thought_payload_v2(KEY, "alice", &head, &input);
        input.signing_key_id = Some("k1".into());
        input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
        chain.append_thought("alice", input).unwrap();
        chain
            .append("bob", ThoughtType::Finding, "an unsigned thought")
            .unwrap();
    }
}

/// Real v2 signatures stay valid when a chain written before the entity fields
/// existed is upgraded: nothing a signature covers changes.
#[test]
fn test_real_signatures_survive_a_layout_upgrade() {
    let dir = TempDir::new().unwrap();
    signed_chain(dir.path());
    let original = MentisDb::open_with_key(dir.path(), KEY).unwrap();
    let before = original.verify_signatures();
    assert_eq!((before.signed, before.invalid), (4, 0));
    let thoughts = original.thoughts().to_vec();
    drop(original);

    // Present the same chain, byte for byte in content, in the older layout.
    let path = dir
        .path()
        .join(chain_storage_filename(KEY, StorageAdapterKind::Binary));
    write_schema3_base_as_is(&path, &thoughts).unwrap();

    let outcome = run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap();
    let MigrationOutcome::Migrated(report) = outcome else {
        panic!("a base-layout chain must be migrated");
    };
    assert_eq!(report.signed, 4);
    assert!(
        !report.hash_changed,
        "a pure layout upgrade must not change any hash"
    );

    let migrated = MentisDb::open_with_key(dir.path(), KEY).unwrap();
    let after = migrated.verify_signatures();
    assert_eq!((after.signed, after.valid, after.invalid), (4, 4, 0));
    assert_eq!(
        migrated.head_hash(),
        thoughts.last().map(|t| t.hash.as_str())
    );
}

/// A v2 signature is bound to the previous record's hash; rebuilding hashes
/// would invalidate it, so the migration is refused and the chain untouched.
#[test]
fn test_migration_that_would_break_a_v2_signature_is_refused() {
    let dir = TempDir::new().unwrap();
    let mut thoughts = semantic_chain(5);
    thoughts[2].signing_key_id = Some("k".into());
    thoughts[2].thought_signature = Some(StoredSignature::encode_v2(&[7; 64]));
    let path = dir
        .path()
        .join(chain_storage_filename(KEY, StorageAdapterKind::Binary));
    write_fixture(&path, FixtureFormat::CurrentLegacyHash, &thoughts).unwrap();
    let before = std::fs::read(&path).unwrap();

    let error = run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap_err();
    assert!(
        matches!(error, MigrationError::SignatureWouldBreak { index: 2 }),
        "{error:?}"
    );
    assert!(error.to_string().contains("untouched"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A v1 signature does not cover the chain position, so it survives a rehash.
#[test]
fn test_v1_signature_survives_a_rehash() {
    let dir = TempDir::new().unwrap();
    let mut thoughts = semantic_chain(5);
    thoughts[2].signing_key_id = Some("k".into());
    thoughts[2].thought_signature = Some(vec![9; 64]);
    let path = dir
        .path()
        .join(chain_storage_filename(KEY, StorageAdapterKind::Binary));
    write_fixture(&path, FixtureFormat::CurrentLegacyHash, &thoughts).unwrap();
    let MigrationOutcome::Migrated(report) =
        run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap()
    else {
        panic!("should migrate");
    };
    assert_eq!(report.signed, 1);
    let chain = MentisDb::open_with_key(dir.path(), KEY).unwrap();
    assert_eq!(chain.thoughts()[2].thought_signature, Some(vec![9; 64]));
}
