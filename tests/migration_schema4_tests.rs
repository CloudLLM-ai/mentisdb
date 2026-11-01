//! Regression tests for the schema-4 era found in real chains.
//!
//! MentisDB builds from March and April 2026 stamped `schema_version = 4` on
//! schema-3 records and hashed them with the stored value `4`; records appended
//! afterwards carry `entity_type` but not `source_episode` and were hashed with
//! schema 3 over the chain as a schema-3 loader had rebuilt it. Every release up
//! to 0.10.10 opened such chains. Master briefly refused them (as "newer than
//! supported"), and decoding the later records as the base layout silently
//! shifted their fields. These tests pin the contract: every historical record
//! verifies under the rule that produced it, nothing is rehashed without
//! verification, and only schemas above 4 are refused as newer.

use mentisdb::migration::fixtures::{
    expected_after_migration, semantic_chain, write_fixture, FixtureFormat,
};
use mentisdb::migration::{
    detect_migration, run_chain_migration, verify_chain_file, MigrationError, MigrationOptions,
    MigrationOutcome, MigrationReason,
};
use mentisdb::{
    chain_storage_filename, MentisDb, StorageAdapterKind, MENTISDB_CURRENT_VERSION,
    MENTISDB_LEGACY_SCHEMA_V4_ALIAS,
};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const KEY: &str = "legacy4";

fn live_path(dir: &Path) -> PathBuf {
    dir.join(chain_storage_filename(KEY, StorageAdapterKind::Binary))
}

fn fixture(format: FixtureFormat, count: usize) -> TempDir {
    let dir = TempDir::new().unwrap();
    write_fixture(&live_path(dir.path()), format, &semantic_chain(count)).unwrap();
    dir
}

/// Rewrite the leading `schema_version` varint of every record to `to`
/// (single-byte varints, so framing is kept). The hashes are NOT recomputed, so
/// this models a chain whose hashes were computed under schema 3.
fn restamp_records(path: &Path, to: u8) {
    let mut bytes = std::fs::read(path).unwrap();
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize;
        bytes[offset + 8] = to;
        offset += 8 + len;
    }
    std::fs::write(path, bytes).unwrap();
}

fn assert_same(left: &[mentisdb::Thought], right: &[mentisdb::Thought]) {
    assert_eq!(left.len(), right.len());
    for (a, b) in left.iter().zip(right) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap(),
            "thought {}",
            a.index
        );
    }
}

#[test]
fn test_alias_is_one_above_current_and_taken() {
    assert_eq!(
        MENTISDB_LEGACY_SCHEMA_V4_ALIAS,
        MENTISDB_CURRENT_VERSION + 1
    );
}

/// The shape of the real chains: a prefix stamped 4 and hashed with 4, then a
/// tail with `entity_type`. The loader opens it with every thought, entity type
/// and hash intact, and the migration engine verifies and migrates it to
/// exactly the same chain.
#[test]
fn test_schema4_era_chain_opens_verifies_and_migrates_identically() {
    let dir = fixture(FixtureFormat::Schema4Era, 30);
    let expected = expected_after_migration(FixtureFormat::Schema4Era, &semantic_chain(30));
    assert!(
        expected.iter().filter(|t| t.entity_type.is_some()).count() > 0,
        "the tail must carry entity types"
    );

    let opened = MentisDb::open_with_key(dir.path(), KEY).expect("era chain opens");
    assert!(opened.verify_integrity());
    assert_same(opened.thoughts(), &expected);
    drop(opened);

    assert_eq!(
        detect_migration(dir.path(), KEY).unwrap(),
        Some(MigrationReason::LegacySchema(4))
    );
    let MigrationOutcome::Migrated(report) =
        run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap()
    else {
        panic!("should migrate");
    };
    assert_eq!(report.thoughts, 30);
    assert!(
        report.hash_changed,
        "schema-4 hashes become schema-3 hashes"
    );
    verify_chain_file(&live_path(dir.path())).unwrap();
    let migrated = MentisDb::open_with_key(dir.path(), KEY).unwrap();
    assert_same(migrated.thoughts(), &expected);
    assert_eq!(
        detect_migration(dir.path(), KEY).unwrap(),
        None,
        "migrated chains need nothing"
    );
}

/// Verification is real: a prefix record whose content was altered under the
/// stored schema-4 hash is refused, in the loader and in the engine, and the
/// file is untouched.
#[test]
fn test_tampered_schema4_record_is_refused_not_rehashed() {
    let dir = fixture(FixtureFormat::Schema4Era, 30);
    let path = live_path(dir.path());
    let mut bytes = std::fs::read(&path).unwrap();
    let needle = b"fixture thought number 5 ";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("record 5 content");
    bytes[at + needle.len() - 2] = b'6'; // "...number 6 ..."
    std::fs::write(&path, &bytes).unwrap();

    assert!(MentisDb::open_with_key(dir.path(), KEY).is_err());
    let error = run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap_err();
    assert!(
        matches!(error, MigrationError::Integrity { index: 5, .. }),
        "{error:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// A tampered tail record is refused too.
#[test]
fn test_tampered_tail_of_schema4_era_chain_is_refused() {
    let dir = fixture(FixtureFormat::Schema4Era, 30);
    let path = live_path(dir.path());
    let mut bytes = std::fs::read(&path).unwrap();
    let needle = b"fixture thought number 25 ";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("record 25 content");
    bytes[at + needle.len() - 2] = b'6';
    std::fs::write(&path, &bytes).unwrap();
    let error = run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap_err();
    assert!(
        matches!(error, MigrationError::Integrity { index: 25, .. }),
        "{error:?}"
    );
}

/// A chain restamped 4 without recomputing hashes (hashes under schema 3) also
/// opens and migrates: the alias is accepted whichever schema produced the hash.
#[test]
fn test_restamped_chains_open_and_migrate() {
    for format in [FixtureFormat::Current, FixtureFormat::Schema3Base] {
        let dir = fixture(format, 12);
        restamp_records(&live_path(dir.path()), 4);
        let chain = MentisDb::open_with_key(dir.path(), KEY).expect("opens");
        assert_eq!(chain.thoughts().len(), 12);
        assert!(chain.verify_integrity());
        drop(chain);
        assert_eq!(
            detect_migration(dir.path(), KEY).unwrap(),
            Some(MigrationReason::LegacySchema(4))
        );
        run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap();
        verify_chain_file(&live_path(dir.path())).unwrap();
    }
}

/// Schema 3 with `entity_type` but no `source_episode` migrates with its
/// entity types (decoding it as the base layout used to shift the fields).
#[test]
fn test_entity_only_layout_keeps_entity_types() {
    let dir = fixture(FixtureFormat::Schema3Entity, 20);
    let expected = expected_after_migration(FixtureFormat::Schema3Entity, &semantic_chain(20));
    let opened = MentisDb::open_with_key(dir.path(), KEY).unwrap();
    assert_same(opened.thoughts(), &expected);
    drop(opened);
    run_chain_migration(dir.path(), KEY, &MigrationOptions::default()).unwrap();
    assert_same(
        MentisDb::open_with_key(dir.path(), KEY).unwrap().thoughts(),
        &expected,
    );
}

/// Values above the alias are still refused as newer than supported, by the
/// loader and the engine, and the file is left untouched.
#[test]
fn test_schema_5_is_still_refused_as_future() {
    let dir = fixture(FixtureFormat::Current, 4);
    restamp_records(&live_path(dir.path()), 5);
    let before = std::fs::read(live_path(dir.path())).unwrap();
    assert!(MentisDb::open_with_key(dir.path(), KEY).is_err());
    match detect_migration(dir.path(), KEY) {
        Err(MigrationError::FutureSchema { version }) => assert_eq!(version, 5),
        other => panic!("expected FutureSchema(5), got {other:?}"),
    }
    assert_eq!(std::fs::read(live_path(dir.path())).unwrap(), before);
}
