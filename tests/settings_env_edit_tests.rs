//! Tests for rewriting a `.env` file: [`dotenv_quote`], [`edit_env_text`] and
//! [`update_env_file`]. The invariant under test is that whatever is saved
//! loads back, through the same parser the daemon uses at startup, as exactly
//! what was meant, and that nothing else in the file changes.

use mentisdb::paths::{dotenv_quote, edit_env_text, update_env_file, EnvFileError, EnvValueError};

/// Parse `.env` text with the daemon's loader and return its assignments.
fn parse(text: &str) -> Vec<(String, String)> {
    dotenvy::from_read_iter(text.as_bytes())
        .map(|item| item.unwrap_or_else(|e| panic!("does not parse: {e:?}\n{text}")))
        .collect()
}

/// A tiny deterministic generator, so the property test needs no dependency
/// and a failure reproduces from the printed value.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Plain values stay readable and are not quoted.
#[test]
fn test_plain_values_are_written_unquoted() {
    for value in [
        "x",
        "/var/lib/mentisdb",
        "CloudLLM-ai/mentisdb",
        "0.85",
        "a=b",
        "h:1,2",
    ] {
        assert_eq!(dotenv_quote(value).unwrap(), value);
    }
}

/// An empty value is written as `""`, not as a bare `NAME=`.
#[test]
fn test_empty_value_is_quoted() {
    assert_eq!(dotenv_quote("").unwrap(), "\"\"");
}

/// NUL, CR and LF cannot be represented and are refused with a typed error.
#[test]
fn test_unrepresentable_values_are_rejected() {
    assert_eq!(dotenv_quote("a\0b"), Err(EnvValueError::Nul));
    assert_eq!(dotenv_quote("a\nB=1"), Err(EnvValueError::LineBreak));
    assert_eq!(dotenv_quote("a\rb"), Err(EnvValueError::LineBreak));
}

/// Hand-picked troublemakers, then 20 000 random strings over an alphabet
/// made of the characters the loader treats specially: every one must come
/// back identical, and a second assignment after it must still load.
#[test]
fn test_every_representable_value_round_trips() {
    let alphabet: Vec<char> = "ab1 \t\"'\\$#={}=é-_./".chars().collect();
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut cases: Vec<String> = ["B=x y", "\"", "pa$word#1", "C:\\dir\\", "'", "\\", "$", ""]
        .map(String::from)
        .to_vec();
    for _ in 0..20_000 {
        let len = (next(&mut state) % 12) as usize;
        cases.push(
            (0..len)
                .map(|_| alphabet[(next(&mut state) % alphabet.len() as u64) as usize])
                .collect(),
        );
    }
    for value in cases {
        let text = format!(
            "A={}\nZ=after\n",
            dotenv_quote(&value).expect("representable")
        );
        let parsed = parse(&text);
        assert_eq!(
            parsed,
            vec![
                ("A".to_string(), value.clone()),
                ("Z".to_string(), "after".to_string())
            ],
            "value {value:?} written as {text:?}"
        );
    }
}

/// A well-formed file written by hand, with unquoted values, comments and
/// `export`, keeps loading as before, and an edit changes only its target
/// line: every other line stays byte-identical.
#[test]
fn test_hand_written_file_survives_an_edit_untouched() {
    let original = "\
# MentisDB settings
MENTISDB_DIR=/srv/mentisdb
export MENTISDB_REST_PORT=9472

MENTISDB_DASHBOARD_PIN='single quoted pin'
MENTISDB_VERBOSE=false # chatty
MENTISDB_LOG_FILE = \"/var/log/m.log\"
";
    assert_eq!(edit_env_text(original, &[]).unwrap(), original);
    let before = parse(original);

    let edited = edit_env_text(original, &[("MENTISDB_REST_PORT", Some("9999"))]).unwrap();
    let expected = original.replace("9472", "9999");
    assert_eq!(edited, expected);

    let after = parse(&edited);
    assert_eq!(before.len(), after.len());
    for ((bn, bv), (an, av)) in before.iter().zip(&after) {
        assert_eq!(bn, an);
        if bn != "MENTISDB_REST_PORT" {
            assert_eq!(bv, av, "{bn} changed");
        }
    }
}

/// `NAME=`, `NAME =` and `export NAME=` are all the same assignment: the first
/// is updated in place (keeping `export`) and later ones are removed.
#[test]
fn test_first_assignment_is_updated_and_duplicates_removed() {
    let text = "export A=1\nB=2\nA =stale\nA=staler\n";
    let edited = edit_env_text(text, &[("A", Some("new"))]).unwrap();
    assert_eq!(edited, "export A=new\nB=2\n");
    assert_eq!(parse(&edited)[0], ("A".to_string(), "new".to_string()));
}

/// A name that only occurs inside a comment or as part of a longer name or
/// value is not an assignment, so a new line is appended instead.
#[test]
fn test_lookalikes_are_not_matched() {
    let text = "# A=1\nAB=2\nB=A=3\n  # export A=4\n";
    let edited = edit_env_text(text, &[("A", Some("9"))]).unwrap();
    assert_eq!(edited, format!("{text}A=9\n"));
}

/// Removing a name deletes every assignment of it and nothing else.
#[test]
fn test_unset_removes_all_assignments() {
    let edited = edit_env_text("A=1\nB=2\nexport A=3\n", &[("A", None)]).unwrap();
    assert_eq!(edited, "B=2\n");
    assert_eq!(edit_env_text("B=2\n", &[("A", None)]).unwrap(), "B=2\n");
}

/// A quoted value that spans lines is one entry. Editing its variable replaces
/// all of it, and a continuation line that looks like `A=` is not mistaken
/// for an assignment of `A`.
#[test]
fn test_multi_line_quoted_values_are_one_entry() {
    let text = "NOTE=\"first\nA=not an assignment\nlast\"\nA=1\n";
    assert_eq!(parse(text).len(), 2);

    let edited = edit_env_text(text, &[("A", Some("2"))]).unwrap();
    assert_eq!(edited, "NOTE=\"first\nA=not an assignment\nlast\"\nA=2\n");

    let edited = edit_env_text(text, &[("NOTE", Some("short"))]).unwrap();
    assert_eq!(edited, "NOTE=short\nA=1\n");
}

/// CRLF files and a UTF-8 byte-order mark load today and keep loading.
#[test]
fn test_crlf_and_bom_files_are_handled() {
    let edited = edit_env_text("\u{feff}A=1\r\nB=2\r\n", &[("B", Some("3"))]).unwrap();
    assert_eq!(edited, "\u{feff}A=1\nB=3\n");
    assert_eq!(
        parse(edited.strip_prefix('\u{feff}').unwrap()).len(),
        2,
        "the rewritten body loads"
    );
}

/// A bad name or value fails the whole edit and returns no text, so a
/// half-edited file cannot be produced.
#[test]
fn test_invalid_names_and_values_fail_the_whole_edit() {
    assert!(matches!(
        edit_env_text("", &[("A", Some("1")), ("bad name", Some("1"))]),
        Err(EnvFileError::InvalidName(_))
    ));
    assert!(matches!(
        edit_env_text("", &[("A", Some("1")), ("B", Some("x\ny"))]),
        Err(EnvFileError::InvalidValue { .. })
    ));
}

/// `update_env_file` creates a missing file, writes nothing when nothing
/// changes, refuses to replace a file it cannot read, and follows a symlink
/// instead of replacing the link.
#[test]
fn test_update_env_file_edge_cases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");

    update_env_file(&path, &[("A", None)]).unwrap();
    assert!(
        !path.exists(),
        "removing from a missing file creates nothing"
    );

    update_env_file(&path, &[("A", Some("1"))]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\n");

    std::fs::write(&path, [b'A', b'=', 0xff, b'\n']).unwrap();
    let err = update_env_file(&path, &[("B", Some("2"))]).unwrap_err();
    assert!(matches!(err, EnvFileError::Read { .. }), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), [b'A', b'=', 0xff, b'\n']);

    #[cfg(unix)]
    {
        let real = dir.path().join("real.env");
        let link = dir.path().join("link.env");
        std::fs::write(&real, "A=1\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        update_env_file(&link, &[("A", Some("2"))]).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "A=2\n");
    }
}
