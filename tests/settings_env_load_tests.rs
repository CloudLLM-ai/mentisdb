//! Tests for loading `.env` files at startup: a file the parser rejects must
//! be reported (and the daemon must refuse to start), not skipped.
//!
//! Each test uses variable names of its own so they can run in parallel in
//! one process; only [`test_try_load_dotenv_files_fails_on_a_bad_mentisdb_dir_env`]
//! touches `MENTISDB_DIR`.

use std::sync::Mutex;

use mentisdb::paths::{load_dotenv_file, load_dotenv_files, try_load_dotenv_files, EnvFileError};

/// Serialises the tests that change the working directory or `MENTISDB_DIR`,
/// which `try_load_dotenv_files` reads.
static PROCESS_STATE: Mutex<()> = Mutex::new(());

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
        // The path is random text that can contain a short bad line such as
        // `B`, so look for the line in the message without the path.
        let without_path = message.replace(&path.display().to_string(), "");
        assert!(
            !without_path.contains("s3cret") && !without_path.contains(bad),
            "{message}"
        );
    }
}

/// An unterminated quote swallows the rest of the file, `MENTISDB_` settings
/// included; the error still names the line where the quote opened.
#[test]
fn test_unterminated_quote_is_reported_at_its_opening_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "A=1\nB=\"never closed\nMENTISDB_TEST_LOAD_C=2\n").unwrap();
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

/// `try_load_dotenv_files` is what the daemon calls: a bad
/// `<MENTISDB_DIR>/.env` makes it fail instead of returning the files that
/// loaded.
#[test]
fn test_try_load_dotenv_files_fails_on_a_bad_mentisdb_dir_env() {
    let _state = PROCESS_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "MENTISDB_TEST_LOAD_FILES=1\nB=x y\nMENTISDB_DASHBOARD_PIN=never-loaded\n",
    )
    .unwrap();
    std::env::set_var("MENTISDB_DIR", dir.path());

    let err = try_load_dotenv_files().unwrap_err();

    assert!(
        matches!(err, EnvFileError::Parse { line: Some(2), .. }),
        "{err}"
    );
    assert!(std::env::var("MENTISDB_DASHBOARD_PIN").is_err());

    // The library's compatible `load_dotenv_files` (0.12.2 signature) warns
    // and carries on: the bad file is not reported as loaded. It shares this
    // test because only one test may set `MENTISDB_DIR`.
    let loaded = load_dotenv_files();
    assert!(!loaded.contains(&dir.path().join(".env")), "{loaded:?}");
}

/// A bad line is fatal only when a `MENTISDB_` assignment (plain or
/// `export`ed) stands on or after it: that is what the loader dropped.
#[test]
fn test_bad_line_is_fatal_only_when_a_mentisdb_setting_follows() {
    let fatal = [
        "B=x y\nMENTISDB_TEST_M2_A=1\n",
        "B=x y\nexport MENTISDB_TEST_M2_A=1\n",
        "MENTISDB_TEST_M2_A=x y\n",
        "\u{feff}MENTISDB_TEST_M2_A=x y\n",
        "B=x y\r\n  MENTISDB_TEST_M2_A =1\r\n",
    ];
    for text in fatal {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, text).unwrap();
        let err = load_dotenv_file(&path).expect_err(text);
        assert!(matches!(err, EnvFileError::Parse { .. }), "{text:?}: {err}");
    }
    let harmless = [
        "OTHER_TEST_M2=1\nB=x y\nOTHER_TEST_M2_B=2\n",
        "MENTISDB_TEST_M2_BEFORE=1\nB=x y\n# MENTISDB_TEST_M2_A=1\nNOT_MENTISDB_TEST_M2=2\n",
    ];
    for text in harmless {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, text).unwrap();
        assert!(load_dotenv_file(&path).expect(text), "{text:?}");
    }
    // What parsed before the bad line was applied; what follows was not.
    assert_eq!(std::env::var("OTHER_TEST_M2").as_deref(), Ok("1"));
    assert!(std::env::var("OTHER_TEST_M2_B").is_err());
    assert_eq!(std::env::var("MENTISDB_TEST_M2_BEFORE").as_deref(), Ok("1"));
}

/// The daemon's loader no longer dies on another tool's `.env` in a parent
/// directory: an unparsable ancestor file with nothing of ours behind the bad
/// line loads as far as it parses, an unreadable one is skipped with a
/// warning, and the real `<MENTISDB_DIR>/.env` still loads. A `MENTISDB_`
/// line behind the bad line stays fatal, in the ancestor file as anywhere.
#[test]
fn test_try_load_dotenv_files_tolerates_a_foreign_ancestor_env() {
    let _state = PROCESS_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    // The working directory is reported canonicalised (/private/var on macOS).
    let root_path = root.path().canonicalize().unwrap();
    let cwd = root_path.join("work/deep");
    std::fs::create_dir_all(&cwd).unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join(".env"), "MENTISDB_TEST_M2_DATA=ok\n").unwrap();
    std::env::set_var("MENTISDB_DIR", data.path());
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(&cwd).unwrap();
    let ancestor = root_path.join(".env");

    std::fs::write(
        &ancestor,
        "FOREIGN_TEST_M2=1\nB=x y\nFOREIGN_TEST_M2_LATER=2\n",
    )
    .unwrap();
    let tolerated = try_load_dotenv_files();

    std::fs::write(&ancestor, "B=x y\nMENTISDB_TEST_M2_PIN=secret\n").unwrap();
    let fatal = try_load_dotenv_files();

    std::env::set_current_dir(previous).unwrap();
    let loaded = tolerated.expect("a foreign bad line must not stop the daemon");
    assert_eq!(loaded, [ancestor.clone(), data.path().join(".env")]);
    assert_eq!(std::env::var("MENTISDB_TEST_M2_DATA").as_deref(), Ok("ok"));
    assert_eq!(std::env::var("FOREIGN_TEST_M2").as_deref(), Ok("1"));
    assert!(std::env::var("FOREIGN_TEST_M2_LATER").is_err());
    assert!(
        matches!(fatal, Err(EnvFileError::Parse { line: Some(1), .. })),
        "{fatal:?}"
    );
}

/// An unreadable `.env` fails closed when it is the working directory's own,
/// and only warns when it was found in a parent directory.
#[cfg(unix)]
#[test]
fn test_unreadable_env_is_fatal_in_the_working_dir_but_not_in_a_parent() {
    use std::os::unix::fs::PermissionsExt;
    let _state = PROCESS_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("MENTISDB_DIR", data.path());
    let locked = |path: &std::path::Path| {
        std::fs::write(path, "MENTISDB_TEST_M2_LOCKED=1\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::read(path).is_err()
    };
    if !locked(&root.path().join(".env")) {
        return; // running as root: mode 000 does not stop the read
    }
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(&cwd).unwrap();

    let parent_only = try_load_dotenv_files();
    assert!(locked(&cwd.join(".env")));
    let own = try_load_dotenv_files();

    std::env::set_current_dir(previous).unwrap();
    assert_eq!(
        parent_only.expect("a parent's unreadable file only warns"),
        Vec::<std::path::PathBuf>::new()
    );
    assert!(matches!(own, Err(EnvFileError::Read { .. })), "{own:?}");
}
