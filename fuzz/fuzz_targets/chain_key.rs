#![no_main]
//! Fuzzes chain-key validation: the same verdict from the library and the
//! server's normalizer, and everything accepted is a visible, bounded,
//! file-name-safe identifier.

use libfuzzer_sys::fuzz_target;
use mentisdb::server::normalize_chain_key;
use mentisdb::{chain_storage_filename, validate_chain_key, StorageAdapterKind, MAX_CHAIN_KEY_BYTES};

fuzz_target!(|data: &[u8]| {
    let Ok(key) = std::str::from_utf8(data) else { return };
    match normalize_chain_key(key) {
        Ok(normalized) => {
            // Trimmed, idempotent and accepted by the library.
            assert_eq!(normalized, normalized.trim());
            assert_eq!(normalize_chain_key(&normalized).unwrap(), normalized);
            validate_chain_key(&normalized).expect("normalizer output is valid");
            assert!(!normalized.is_empty() && normalized.len() <= MAX_CHAIN_KEY_BYTES);
            assert!(!normalized.chars().any(char::is_control));
            // The storage file name never escapes the chain directory.
            let file = chain_storage_filename(&normalized, StorageAdapterKind::Binary);
            assert!(!file.contains('/') && !file.contains('\\') && !file.contains(".."));
            assert!(!file.starts_with('-') || normalized.chars().next() == Some('-'));
        }
        Err(_) => {
            // A rejected key is rejected by the library too once trimmed.
            assert!(validate_chain_key(key.trim()).is_err());
        }
    }
});
