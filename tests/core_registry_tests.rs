//! Regression tests for atomic, race-free chain registry persistence.

use mentisdb::{load_registered_chains, MentisDb, ThoughtType};
use std::sync::Arc;
use tempfile::tempdir;

/// Concurrent chain opens must not lose registry entries (read-modify-write
/// race) or ever leave an unparsable registry file behind.
#[test]
fn test_concurrent_chain_registration_keeps_every_chain() {
    const CHAINS: usize = 16;
    let dir = Arc::new(tempdir().unwrap());
    let handles: Vec<_> = (0..CHAINS)
        .map(|i| {
            let dir = Arc::clone(&dir);
            std::thread::spawn(move || {
                let mut chain = MentisDb::open_with_key(dir.path(), &format!("chain-{i}")).unwrap();
                chain
                    .append("agent", ThoughtType::Finding, "hello")
                    .unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    let registry = load_registered_chains(dir.path()).unwrap();
    for i in 0..CHAINS {
        assert!(
            registry.chains.contains_key(&format!("chain-{i}")),
            "chain-{i} missing from registry"
        );
    }
    let stray = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
        .count();
    assert_eq!(stray, 0, "temp files must be renamed away");
}
