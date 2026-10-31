//! Tests for `mentisdb key generate|show`.

use mentisdb::cli::run_with_io;
use std::io::Cursor;

fn cli(args: &[&str]) -> (bool, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let argv = std::iter::once("mentisdb").chain(args.iter().copied());
    let code = run_with_io(argv, &mut Cursor::new(""), &mut out, &mut err);
    (
        format!("{code:?}") == format!("{:?}", std::process::ExitCode::SUCCESS),
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn public_of(output: &str) -> String {
    output
        .lines()
        .find_map(|l| l.strip_prefix("public_key_hex: "))
        .expect("public key line")
        .to_string()
}

/// A generated key is saved owner-only, shows the same public key, differs per run, and is never overwritten silently.
#[test]
fn test_generate_save_show_and_overwrite_protection() {
    let dir = std::env::temp_dir().join(format!("mentisdb_key_cmd_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.key");
    let path = file.to_str().unwrap();

    let (ok, out, err) = cli(&["key", "generate", "--out", path]);
    assert!(ok, "{err}");
    let public = public_of(&out);
    assert_eq!(public.len(), 64);
    let seed = std::fs::read_to_string(&file).unwrap();
    assert_eq!(seed.trim().len(), 64);
    assert!(
        !out.contains(seed.trim()),
        "the seed must not be printed when saved"
    );
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&file).unwrap().permissions())
            & 0o777,
        0o600
    );

    let (ok, shown, _) = cli(&["key", "show", path]);
    assert!(ok);
    assert_eq!(public_of(&shown), public);

    let (ok, _, err) = cli(&["key", "generate", "--out", path]);
    assert!(!ok && err.contains("--force"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        seed,
        "file must be untouched"
    );
    let (ok, forced, _) = cli(&["key", "generate", "--out", path, "--force"]);
    assert!(ok);
    assert_ne!(public_of(&forced), public);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without --out the seed is printed once; bad input is reported.
#[test]
fn test_generate_without_out_prints_seed_and_errors_are_clear() {
    let (ok, out, _) = cli(&["key", "generate"]);
    assert!(ok && out.contains("private_key_seed_hex: "));
    let (ok, _, err) = cli(&["key", "show", "/nonexistent/key"]);
    assert!(!ok && err.contains("cannot read"));
    let (ok, _, err) = cli(&["key", "bogus"]);
    assert!(!ok && err.contains("Unknown subcommand 'bogus'"), "{err}");
    let (ok, out, _) = cli(&["key", "--help"]);
    assert!(ok && out.contains("mentisdb key generate"));
}
