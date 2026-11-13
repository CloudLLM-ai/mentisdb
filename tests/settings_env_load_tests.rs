//! Tests for loading `.env` files at startup: a file the parser rejects must
//! be reported (and the daemon must refuse to start), not skipped.
//!
//! Each test uses variable names of its own so they can run in parallel in
//! one process; only [`test_load_dotenv_files_fails_on_a_bad_mentisdb_dir_env`]
//! touches `MENTISDB_DIR`.

use mentisdb::paths::{load_dotenv_file, load_dotenv_files, EnvFileError};

/// Lines `dotenvy` rejects, each of which used to abort the file silently at
/// that line and leave every later variable unset.
const BAD_LINES: &[&str] = &[
    "B=x y", "B=\"", "B='", "B=a \"b", "B=a\\q", "=novalue", "1B=x", "B",
];

/// A file whose first bad line comes before the security settings reports the
/// error, names the line, and does not echo the secret on it.
#[test]
fn test_bad_line_is_an_error_naming_the_line_without_echoing_it() {
    for bad in BAD_LINES {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            format!("# header\nMENTISDB_TEST_LOAD_BAD_FIRST=1\n{bad}\nMENTISDB_TEST_LOAD_BAD_PIN=s3cret\n"),
        )
        .unwrap();

        let err = load_dotenv_file(&path).expect_err(bad);

        match &err {
            EnvFileError::Parse { line, path: p } => {
                assert_eq!(*line, Some(3), "{bad:?}");
                assert_eq!(p, &path);
            }
            other => panic!("{bad:?}: expected Parse, got {other}"),
        }
        let message = err.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("line 3"), "{message}");
        assert!(
            !message.contains("s3cret") && !message.contains(bad),
            "{message}"
        );
    }
}

/// An unterminated quote swallows the rest of the file; the error still names
/// the line where the quote opened.
#[test]
fn test_unterminated_quote_is_reported_at_its_opening_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "A=1\nB=\"never closed\nC=2\n").unwrap();
    let err = load_dotenv_file(&path).unwrap_err();
    assert!(
        matches!(err, EnvFileError::Parse { line: Some(2), .. }),
        "{err}"
    );
}

/// A well-formed file loads, never overrides a variable that is already set,
/// and reports that it was loaded; a missing file is `Ok(false)`.
#[test]
fn test_good_file_loads_without_overriding_and_missing_file_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(
        &path,
        "MENTISDB_TEST_LOAD_NEW=\"a b\"\nexport MENTISDB_TEST_LOAD_SET=from-file\nMENTISDB_TEST_LOAD_PLAIN=x\n",
    )
    .unwrap();
    std::env::set_var("MENTISDB_TEST_LOAD_SET", "from-process");

    assert!(load_dotenv_file(&path).unwrap());

    assert_eq!(
        std::env::var("MENTISDB_TEST_LOAD_NEW").as_deref(),
        Ok("a b")
    );
    assert_eq!(
        std::env::var("MENTISDB_TEST_LOAD_PLAIN").as_deref(),
        Ok("x")
    );
    assert_eq!(
        std::env::var("MENTISDB_TEST_LOAD_SET").as_deref(),
        Ok("from-process")
    );
    assert!(!load_dotenv_file(&dir.path().join("absent.env")).unwrap());
}

/// A path that exists but cannot be read (a directory here) is a `Read`
/// error, not a silent skip.
#[test]
fn test_unreadable_file_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_dotenv_file(dir.path()).unwrap_err();
    assert!(matches!(err, EnvFileError::Read { .. }), "{err}");
}

/// `load_dotenv_files` is what the daemon calls: a bad
/// `<MENTISDB_DIR>/.env` makes it fail instead of returning the files that
/// loaded.
#[test]
fn test_load_dotenv_files_fails_on_a_bad_mentisdb_dir_env() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "MENTISDB_TEST_LOAD_FILES=1\nB=x y\nMENTISDB_DASHBOARD_PIN=never-loaded\n",
    )
    .unwrap();
    std::env::set_var("MENTISDB_DIR", dir.path());

    let err = load_dotenv_files().unwrap_err();

    assert!(
        matches!(err, EnvFileError::Parse { line: Some(2), .. }),
        "{err}"
    );
    assert!(std::env::var("MENTISDB_DASHBOARD_PIN").is_err());
}
