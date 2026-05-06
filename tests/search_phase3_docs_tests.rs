#[test]
fn readme_marks_phase3_as_complete_and_optional() {
    let readme = include_str!("../README.md");

    assert!(readme.contains("MentisDB now exposes an additive Phase 3 vector sidecar surface"));
    assert!(readme.contains("embeddings remain optional"));
    assert!(readme.contains("vector state lives in a rebuildable sidecar"));
    assert!(readme.contains("managed vector sidecar"));
    assert!(readme.contains("`mentisdb` now applies a persisted managed-vector setting"));
    assert!(readme.contains("local-text-v1"));
    assert!(
        readme.contains("vector hits surface whether they came from a `Fresh` or stale sidecar")
    );
    assert!(!readme.contains("has **not started yet**"));
}
