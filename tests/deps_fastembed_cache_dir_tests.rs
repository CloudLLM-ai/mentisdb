//! Verifies fastembed cache-directory resolution without downloading a model.

#![cfg(feature = "local-embeddings")]

use std::path::{Path, PathBuf};

use mentisdb::search::fastembed_provider::resolve_cache_dir;

/// Without an explicit override the cache lives under the MentisDB directory.
#[test]
fn test_cache_dir_defaults_under_mentisdb_dir() {
    assert_eq!(
        resolve_cache_dir(None, Path::new("/data/mdb")),
        PathBuf::from("/data/mdb/.fastembed_cache")
    );
}

/// An explicit `FASTEMBED_CACHE_DIR` wins; an empty one is ignored.
#[test]
fn test_cache_dir_honours_explicit_override() {
    assert_eq!(
        resolve_cache_dir(Some(PathBuf::from("/models")), Path::new("/data/mdb")),
        PathBuf::from("/models")
    );
    assert_eq!(
        resolve_cache_dir(Some(PathBuf::new()), Path::new("/data/mdb")),
        PathBuf::from("/data/mdb/.fastembed_cache")
    );
}
