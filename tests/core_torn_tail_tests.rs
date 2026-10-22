//! Regression tests for torn-tail recovery of binary chain files.

use mentisdb::{MentisDb, ThoughtType};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn chain_path(chain: &MentisDb) -> PathBuf {
    PathBuf::from(chain.storage_location())
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn write_chain(dir: &Path, count: usize) -> PathBuf {
    let mut chain = MentisDb::open_with_key(dir, "torn").unwrap();
    for i in 0..count {
        chain
            .append("agent", ThoughtType::Finding, &format!("thought {i}"))
            .unwrap();
    }
    let path = chain_path(&chain);
    chain.flush().unwrap();
    path
}

/// Appends garbage after a valid chain, reopens, appends, reopens again and
/// asserts every good thought still verifies and the new one is aligned.
fn assert_survives_garbage_tail(garbage: &[u8]) {
    let temp = tempdir().unwrap();
    let path = write_chain(temp.path(), 3);
    let good_len = std::fs::metadata(&path).unwrap().len();
    append_bytes(&path, garbage);

    {
        let mut chain = MentisDb::open_with_key(temp.path(), "torn").unwrap();
        assert_eq!(chain.thoughts().len(), 3, "good prefix must still load");
        chain
            .append("agent", ThoughtType::Finding, "after the tear")
            .unwrap();
        chain.flush().unwrap();
    }

    let chain = MentisDb::open_with_key(temp.path(), "torn").unwrap();
    assert_eq!(chain.thoughts().len(), 4);
    assert!(chain.verify_integrity());
    assert_eq!(chain.thoughts()[3].content, "after the tear");
    assert!(std::fs::metadata(&path).unwrap().len() > good_len);

    let preserved = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".torn-"))
        .count();
    assert_eq!(preserved, 1, "dropped tail is preserved beside the chain");
}

/// A partial 8-byte length prefix at EOF must not misalign later appends.
#[test]
fn test_partial_length_prefix_tail_is_truncated_before_append() {
    assert_survives_garbage_tail(&[0xAA, 0xBB, 0xCC]);
}

/// A payload shorter than its prefix at EOF must not make open fail or
/// misalign later appends.
#[test]
fn test_partial_payload_tail_is_truncated_before_append() {
    let mut garbage = 100_u64.to_le_bytes().to_vec();
    garbage.extend_from_slice(&[7; 10]);
    assert_survives_garbage_tail(&garbage);
}

/// An intact chain is left byte-for-byte untouched by the repair step.
#[test]
fn test_intact_chain_is_not_modified_on_append_open() {
    let temp = tempdir().unwrap();
    let path = write_chain(temp.path(), 2);
    let before = std::fs::read(&path).unwrap();
    {
        let mut chain = MentisDb::open_with_key(temp.path(), "torn").unwrap();
        chain.append("agent", ThoughtType::Finding, "more").unwrap();
        chain.flush().unwrap();
    }
    let after = std::fs::read(&path).unwrap();
    assert!(after.starts_with(&before));
    assert!(std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().contains(".torn-")));
}

/// A corrupted length prefix on a middle record must fail open with
/// `InvalidData` and leave the file byte-identical, never truncating the
/// valid records that follow it.
#[test]
fn test_corrupt_middle_length_prefix_fails_open_without_truncation() {
    let temp = tempdir().unwrap();
    let path = write_chain(temp.path(), 6);
    let mut bytes = std::fs::read(&path).unwrap();
    let first_len = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
    let second = 8 + first_len;
    bytes[second..second + 8].copy_from_slice(&9_000_000_u64.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();

    let result = MentisDb::open_with_key(temp.path(), "torn");
    assert!(result.is_err(), "corrupt middle prefix must not open");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "file must be untouched"
    );
    assert!(std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().contains(".torn-")));
}

/// The preserved `.torn-*` file must be private (0600) on unix.
#[cfg(unix)]
#[test]
fn test_torn_tail_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let path = write_chain(temp.path(), 2);
    append_bytes(&path, &[1, 2, 3]);
    let mut chain = MentisDb::open_with_key(temp.path(), "torn").unwrap();
    chain.append("agent", ThoughtType::Finding, "x").unwrap();
    chain.flush().unwrap();
    let torn = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().contains(".torn-"))
        .expect("torn file preserved");
    let mode = torn.metadata().unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
