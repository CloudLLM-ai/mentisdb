#![no_main]
//! Fuzzes a whole chain file through the migration dry run.
//!
//! The dry run streams the file through the same decode, upgrade and
//! hash-rebuild code as a real migration. Invariants: it never panics, it
//! never modifies the input file, it never creates the migrations directory,
//! and a chain it accepts reports a consistent thought count.

use libfuzzer_sys::fuzz_target;
use mentisdb::migration::{detect_migration, dry_run_chain, MigrationOptions};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use std::path::PathBuf;
use std::sync::OnceLock;

fn scratch() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("mentisdb-fuzz-migration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    })
}

fuzz_target!(|data: &[u8]| {
    let dir = scratch();
    for kind in [StorageAdapterKind::Binary, StorageAdapterKind::Jsonl] {
        let path = dir.join(chain_storage_filename("fuzz", kind));
        let other = dir.join(chain_storage_filename(
            "fuzz",
            if kind == StorageAdapterKind::Binary {
                StorageAdapterKind::Jsonl
            } else {
                StorageAdapterKind::Binary
            },
        ));
        let _ = std::fs::remove_file(&other);
        std::fs::write(&path, data).expect("write input");
        let _ = detect_migration(dir, "fuzz");
        if let Ok(Some(report)) = dry_run_chain(dir, "fuzz", &MigrationOptions::default()) {
            assert!(report.staged_bytes >= report.thoughts, "every record has a length prefix");
        }
        assert_eq!(std::fs::read(&path).expect("input still there"), data, "dry run must not modify the input");
        assert!(!dir.join("migrations").exists(), "dry run must not create state");
    }
});
