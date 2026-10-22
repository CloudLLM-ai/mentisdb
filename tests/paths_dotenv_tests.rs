use mentisdb::paths::{load_dotenv_files, mentisdb_dir_env_file};

/// Settings saved by the dashboard to `<MENTISDB_DIR>/.env` must be loaded at
/// startup even when the daemon runs from a different working directory, and
/// must never override variables already present in the process environment.
#[test]
fn test_load_dotenv_files_reads_mentisdb_dir_env_without_overriding() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(".env"),
        "MENTISDB_TEST_DOTENV_NEW=from-file\nMENTISDB_TEST_DOTENV_SET=from-file\n",
    )
    .expect("write .env");
    // This test binary holds a single test, so nothing else touches the
    // process environment concurrently.
    std::env::set_var("MENTISDB_DIR", dir.path());
    std::env::set_var("MENTISDB_TEST_DOTENV_SET", "from-process");
    std::env::remove_var("MENTISDB_TEST_DOTENV_NEW");

    assert_eq!(mentisdb_dir_env_file(), dir.path().join(".env"));
    let loaded = load_dotenv_files();

    assert!(loaded.contains(&dir.path().join(".env")));
    assert_eq!(
        std::env::var("MENTISDB_TEST_DOTENV_NEW").as_deref(),
        Ok("from-file")
    );
    assert_eq!(
        std::env::var("MENTISDB_TEST_DOTENV_SET").as_deref(),
        Ok("from-process")
    );
}
