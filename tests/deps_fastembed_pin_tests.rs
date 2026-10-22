//! Guards the fastembed major-version pin.
//!
//! fastembed 7 depends on ort 2.0.0-rc.15, which auto-registers the CoreML
//! execution provider on macOS and offers no public switch to disable it
//! through fastembed. Query embedding became ~2.5x slower (LoCoMo 363 -> 141
//! q/s) and the vectors are no longer produced by the CPU provider that wrote
//! existing sidecars. Until the execution provider can be pinned to CPU, the
//! dependency must stay on 5.x.

/// `Cargo.toml` must keep fastembed on the 5.x line.
#[test]
fn test_fastembed_manifest_requirement_stays_on_5x() {
    let manifest = include_str!("../Cargo.toml");
    let line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("fastembed ="))
        .expect("fastembed dependency line");
    assert!(
        line.contains("version = \"5."),
        "fastembed must stay on 5.x (see this file's docs), got: {line}"
    );
}

/// The resolved lockfile entry must also be a 5.x release.
#[test]
fn test_fastembed_locked_version_is_5x() {
    let lock = include_str!("../Cargo.lock");
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "name = \"fastembed\"" {
            let version = lines.next().expect("version line after name");
            assert!(
                version.trim().starts_with("version = \"5."),
                "locked fastembed must be 5.x, got: {version}"
            );
            return;
        }
    }
    panic!("fastembed not found in Cargo.lock");
}
