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

/// Bad input is reported and the help is reachable.
#[test]
fn test_errors_are_clear() {
    let (ok, _, err) = cli(&["key", "show", "/nonexistent/key"]);
    assert!(!ok && err.contains("cannot read"));
    let (ok, _, err) = cli(&["key", "bogus"]);
    assert!(!ok && err.contains("Unknown subcommand 'bogus'"), "{err}");
    let (ok, out, _) = cli(&["key", "--help"]);
    assert!(ok && out.contains("mentisdb key generate"));
}

/// `MENTISDB_KEY_DIR` is process-global; the tests that set it take turns.
static KEY_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn key_dir_guard() -> std::sync::MutexGuard<'static, ()> {
    KEY_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions()) & 0o777
}

/// Regression: `key generate` without `--out` printed the private seed on
/// standard output, which lands in terminal scrollback and, for an agent that
/// runs the command, in its context. It now saves the seed to an owner-only
/// file in the key directory and prints only the public key and the path.
#[test]
fn test_generate_without_out_never_prints_the_seed() {
    let _guard = key_dir_guard();
    let dir = std::env::temp_dir().join(format!("mentisdb_keydir_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let keys = dir.join("keys");
    std::env::set_var("MENTISDB_KEY_DIR", &keys);

    let (ok, out, err) = cli(&["key", "generate"]);
    let (ok2, out2, _) = cli(&["key", "generate"]);
    std::env::remove_var("MENTISDB_KEY_DIR");
    assert!(ok && ok2, "{err}");

    let public = public_of(&out);
    assert_ne!(public, public_of(&out2), "each run makes a new key");
    let file = keys.join(format!("{}.key", &public[..16]));
    assert!(file.exists(), "saved at {file:?}: {out}");
    let seed = std::fs::read_to_string(&file).unwrap();
    assert_eq!(seed.trim().len(), 64);
    assert!(
        !out.contains(seed.trim()),
        "the seed must never be printed: {out}"
    );
    assert!(!out.contains("private_key_seed_hex"), "{out}");
    assert!(
        out.contains(file.to_str().unwrap()),
        "names the file: {out}"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&keys), 0o700, "the key directory is private");
    }
    let (ok, shown, _) = cli(&["key", "show", file.to_str().unwrap()]);
    assert!(ok);
    assert_eq!(public_of(&shown), public);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--force` over an existing readable file leaves it owner-only: the mode of
/// an existing file survives `open`, so the old code wrote the new seed into a
/// `0644` file.
#[cfg(unix)]
#[test]
fn test_force_makes_an_existing_world_readable_file_private() {
    let dir = std::env::temp_dir().join(format!("mentisdb_key_force_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("open.key");
    std::fs::write(&file, "old").unwrap();
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let (ok, _, err) = cli(&[
        "key",
        "generate",
        "--out",
        file.to_str().unwrap(),
        "--force",
    ]);
    assert!(ok, "{err}");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim().len(), 64);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seed file other users can read is flagged; a private one, a missing one
/// and (without Unix modes) any one is not.
#[cfg(unix)]
#[test]
fn test_seed_file_mode_warning() {
    use mentisdb::signing::seed_file_mode_warning;
    let dir = std::env::temp_dir().join(format!("mentisdb_key_mode_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("k.key");
    std::fs::write(&file, "ab".repeat(32)).unwrap();
    for (bits, warns) in [
        (0o600, false),
        (0o400, false),
        (0o640, true),
        (0o604, true),
        (0o644, true),
    ] {
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(bits))
            .unwrap();
        let warning = seed_file_mode_warning(&file);
        assert_eq!(warning.is_some(), warns, "mode {bits:o}");
        if let Some(warning) = warning {
            assert!(warning.contains("chmod 600"), "{warning}");
            assert!(warning.contains(file.to_str().unwrap()), "{warning}");
        }
    }
    assert!(seed_file_mode_warning(&dir.join("missing")).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The operator key MentisDB creates, and the seed loaders, accept a readable
/// file (they warn, they do not refuse), and the key is a 256-bit OS-random
/// seed: two operator identities never collide.
#[test]
fn test_operator_keys_are_distinct_and_loading_a_readable_one_still_works() {
    let a = std::env::temp_dir().join(format!("mentisdb_op_a_{}", std::process::id()));
    let b = std::env::temp_dir().join(format!("mentisdb_op_b_{}", std::process::id()));
    let ka = mentisdb::operator::OperatorIdentity::load_or_create(&a).unwrap();
    let kb = mentisdb::operator::OperatorIdentity::load_or_create(&b).unwrap();
    assert_ne!(ka.public_key(), kb.public_key());
    #[cfg(unix)]
    {
        let path = mentisdb::operator::OperatorIdentity::key_path(&a);
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        let again = mentisdb::operator::OperatorIdentity::load(&path).unwrap();
        assert_eq!(again.public_key(), ka.public_key());
    }
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}
